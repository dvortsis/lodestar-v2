//! ENR-G, the adult-enrichment gate (architecture §18.7, §18.9, §20.5,
//! §20.9; `docs/decisions/2026-11-24-ENR-G.md`): the real ThePornDB and
//! StashDB plugins, run by the real enrichment runtime over a real store
//! and filter, behind a fake transport that records every request.
//!
//! - **Never looked up:** with Show adult content off, the plugin's
//!   consent missing, the plugin off or its kill switch tripped, nothing is
//!   sent for any item, Adult ones included, and nothing is stored.
//! - **Never sent:** an item the child-safety core holds or a rule hides,
//!   drops or quarantines, and a private one, is never sent, whatever the
//!   settings; no request carries a hash or another item's title.
//! - **Never stored:** an answer for an item blocked after its lookup, or
//!   whose text the core matches, is dropped before storage, and so is one
//!   written after consent was withdrawn; the runtime logs no name, query,
//!   answer or hash.
//! - **Consent:** both plugins are Account class and in the consent gate's
//!   catalogue; withdrawing consent, or switching the plugin off, stops
//!   lookups at once.
//! - **Purge:** purging the source removes its claims and cache, and the
//!   torrent's classification and index entry are derived again without
//!   the match (a running server's case is in
//!   `crates/pipeline/tests/control.rs`).
//! - **Kill switch:** a route that's down for good, or not yet checked,
//!   sends nothing, even if the host's switch says it hasn't tripped.
//!
//! No fixed sleeps: every wait polls a condition with a 10 s deadline.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_classify::{AdultKind, ContentType, ENRICHMENT_VERSION, Record};
use lodestar_filter::{Action, Core, CsList, Handle, Lists, Origin, Switches as Core_, TermSet};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::adult_enrich::{
    AdultEnrichSource, Config, EnrichSources, EnrichStats, EnricherConsent, Interest, Switches,
};
use lodestar_pipeline::enrich::{Backfill, enrich_stored_with, enrich_with_rule, file_infos};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::unverified::{self, Ctx, UnverifiedStats};
use lodestar_sources::enrich::{AdultEnricher, StoreState};
use lodestar_sources::testing::Replayer;
use lodestar_sources::{
    BoxFuture, Consent, Discovery, DownReason, Egress, EgressCredentials, EgressFactory, Emitter,
    Endpoints, Fetch, HttpError, Kit, MemoryState, PluginState, Request, Response, RouteKind,
    RouteState, Shutdown, SocksDns, SourceCtx, SourcePlugin, UserLimits,
};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::catalogue;
use lodestar_sources_plugins::stashdb::{self, StashDb};
use lodestar_sources_plugins::tpdb::{self, Tpdb};
use lodestar_store::unverified::PurgeCursor;
use lodestar_store::{NewTorrent, SourceId, SourceState, Store, StoreConfig, flags};
use tokio::sync::mpsc;

const T: i64 = 1_700_000_000;
const DEADLINE: Duration = Duration::from_secs(10);
/// The name ThePornDB's recorded match answers (`fixtures/tpdb/tpdb_match`).
const MATCHED: &str = "Example.Studio.24.02.29.Synthetic.Scene.One.1080p";

// ---- fixtures ------------------------------------------------------------

/// A single-file info dict named `name` (1 MiB, four pieces).
fn info_of(name: &str) -> Vec<u8> {
    let mut d = b"d6:lengthi1048576e4:name".to_vec();
    d.extend(format!("{}:{name}", name.len()).bytes());
    d.extend(b"12:piece lengthi262144e6:pieces80:");
    d.extend([0u8; 80]);
    d.push(b'e');
    d
}

/// Stores a verified torrent named `name`, found by `source`, with
/// `extra` flags, classified Adult by an Is adult rule.
fn put(s: &mut Store, name: &str, source: SourceId, extra: i64) -> (i64, InfoHash) {
    let raw = info_of(name);
    let info = Info::parse(&raw).unwrap();
    let id = s
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
    s.set_enrichments(&[(id, enrich_with_rule(name, &files, Some(true)))])
        .unwrap();
    (id, info.swarm_hashes().next().unwrap())
}

/// The child-safety core with stand-in lists: `lexa` and `synthetic` are
/// sexual terms, `rolea` and `performer` child roles. So "Rolea Lexa" is a
/// pattern hit, and ThePornDB's recorded answer ("Example Performer One")
/// beside [`MATCHED`] ("Synthetic") is one too, while the name alone isn't.
fn core() -> Arc<Handle> {
    let lists = Lists {
        sexual: TermSet::of(&["lexa", "synthetic"]),
        child_roles: TermSet::of(&["rolea", "performer"]),
        codes: TermSet::of(&["codex"]),
    };
    Arc::new(Handle::new(lodestar_filter::Engine::new(Core::new(
        lists,
        CsList::default(),
        Core_::default(),
    ))))
}

/// A filter whose core has no term lists: only hashes can hold.
fn bare() -> Arc<Handle> {
    let lists = Lists {
        sexual: TermSet::of(&[]),
        child_roles: TermSet::of(&[]),
        codes: TermSet::of(&[]),
    };
    Arc::new(Handle::new(lodestar_filter::Engine::new(Core::new(
        lists,
        CsList::default(),
        Core_::default(),
    ))))
}

fn rules(filter: &Handle, rows: &[(&str, Action)]) {
    let rules = rows
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
            rules,
            ..Default::default()
        });
        assert!(notes.is_empty(), "{notes:?}");
        e.version += 1;
    });
}

/// The two adult-enrichment plugins, with an API key the plugin can read.
fn plugins() -> [Arc<dyn AdultEnricher>; 2] {
    let key = || Arc::new(StaticCredentials::new([("api_key", "REDACTED")]));
    [Arc::new(Tpdb::new(key())), Arc::new(StashDb::new(key()))]
}

/// A store with the DHT, an always-adult source and `plugin`'s source,
/// registered, enabled and consented to (its Account credential stored).
fn store_for(plugin: &str) -> (Store, SourceId, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let sukebei = s.register_source("sukebei", "1", 0).unwrap();
    let enr = s.register_source(plugin, "1", 0).unwrap();
    consent(&mut s, enr, true);
    (s, dht, sukebei, enr)
}

fn consent(s: &mut Store, enr: SourceId, given: bool) {
    if given {
        let state = Some(SourceState::Enabled);
        s.update_source_secrets(enr, &[("api_key", b"sealed")], &[], state)
            .unwrap();
    } else {
        s.update_source_secrets(enr, &[], &["api_key"], None)
            .unwrap();
    }
}

#[derive(Default)]
struct Flags {
    show_adult: AtomicBool,
    tripped: AtomicBool,
}

impl Switches for Flags {
    fn show_adult(&self) -> bool {
        self.show_adult.load(Ordering::SeqCst)
    }
    fn kill_switch_tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }
}

fn flags(show_adult: bool, tripped: bool) -> Arc<Flags> {
    let f = Flags::default();
    f.show_adult.store(show_adult, Ordering::SeqCst);
    f.tripped.store(tripped, Ordering::SeqCst);
    Arc::new(f)
}

/// The fake transport: records every request. With a replay it answers
/// from the recorded fixture; otherwise every request gets a 503, so a
/// lookup that gets this far fails and nothing is matched.
#[derive(Default)]
struct Wire {
    seen: Mutex<Vec<Request>>,
    replay: Option<Replayer>,
    /// Runs as each request is sent, while the lookup is in flight.
    during: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Wire {
    fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

impl Fetch for Wire {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.seen.lock().unwrap().push(req.clone());
        if let Some(f) = &self.during {
            f();
        }
        Box::pin(async move {
            match &self.replay {
                Some(r) => r.fetch(req).await,
                None => Ok(Response {
                    status: 503,
                    headers: Vec::new(),
                    body: Vec::new(),
                }),
            }
        })
    }
}

/// The runtime running as the plugin's source, as the pipeline runs it.
struct Running {
    task: tokio::task::JoinHandle<Result<(), lodestar_sources::PluginError>>,
    stop: tokio::sync::watch::Sender<bool>,
    found: mpsc::Receiver<Discovery>,
    interest: Interest,
    stats: Arc<EnrichStats>,
}

impl Running {
    async fn stop(self) -> mpsc::Receiver<Discovery> {
        let _ = self.stop.send(true);
        self.task.await.unwrap().unwrap();
        self.found
    }
}

struct Setup {
    store: Arc<Mutex<Store>>,
    filter: Arc<Handle>,
    adult: AdultSources,
    switches: Arc<Flags>,
    source: SourceId,
    background: bool,
}

fn start(
    plugin: Arc<dyn AdultEnricher>,
    s: &Setup,
    fetch: Arc<dyn Fetch>,
    state: Arc<dyn PluginState>,
) -> Running {
    let config = Config {
        background: s.background,
        interval: Duration::ZERO,
        idle: Duration::from_millis(10),
        ..Config::default()
    };
    let (src, interest) = AdultEnrichSource::new(
        plugin,
        s.store.clone(),
        s.filter.clone(),
        s.adult,
        s.switches.clone(),
        config,
    )
    .unwrap();
    let stats = src.stats();
    let (tx, found) = mpsc::channel(64);
    let (stop, shutdown) = Shutdown::new();
    let kit = Kit::for_plugin(&src, state, fetch, &UserLimits::default());
    let ctx = SourceCtx {
        source: s.source,
        emitter: Emitter::new(s.source, tx),
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
    }
}

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

fn n(c: &std::sync::atomic::AtomicU64) -> u64 {
    c.load(Ordering::SeqCst)
}

/// Every spelling of `key` a request could carry: hex in either case,
/// base32, and the raw bytes percent-encoded.
fn spellings(key: &InfoHash) -> Vec<String> {
    let hex = key.to_string();
    let pct: String = key.0.iter().map(|b| format!("%{b:02X}")).collect();
    vec![
        hex.to_uppercase(),
        hex,
        base32(&key.0),
        pct.to_lowercase(),
        pct,
    ]
}

fn base32(b: &[u8]) -> String {
    const A: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut out, mut acc, mut bits) = (String::new(), 0u32, 0);
    for &x in b {
        acc = (acc << 8) | u32::from(x);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((acc >> bits) & 31) as usize] as char);
        }
    }
    out
}

/// A request's URL, headers and body, as one lower-case text to search.
fn text_of(r: &Request) -> String {
    let mut t = r.url.clone();
    for (k, v) in &r.headers {
        t.push_str(&format!("\n{k}: {v}"));
    }
    t.push('\n');
    t.push_str(&String::from_utf8_lossy(&r.body));
    t.to_lowercase()
}

// ---- 1. never looked up ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Off {
    ShowAdult,
    Consent,
    Plugin,
    KillSwitch,
}

/// Brief §1 (plan Q11; §20.9, §18.7, SRC-5): with Show adult content off,
/// the plugin's consent not given, the plugin off, or its kill switch
/// tripped, no request is built or sent for any item, Adult and
/// always-adult ones included, by user interest or the background. Nothing
/// is emitted, so nothing can be stored, and the plugin's state is left
/// empty: no cache, no budget, no cursor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_looked_up_while_a_condition_is_off() {
    for plugin in plugins() {
        let id = plugin.manifest().id;
        for off in [Off::ShowAdult, Off::Consent, Off::Plugin, Off::KillSwitch] {
            let (mut s, dht, sukebei, enr) = store_for(id);
            match off {
                Off::Consent => consent(&mut s, enr, false),
                Off::Plugin => s.set_source_state(enr, SourceState::Disabled).unwrap(),
                Off::ShowAdult | Off::KillSwitch => {}
            }
            let mut ids = vec![put(&mut s, MATCHED, dht, 0).0];
            ids.push(put(&mut s, "Studiox Garden Scene XXX 1080p", sukebei, 0).0);
            ids.push(put(&mut s, "Another Example Scene 2160p", dht, 0).0);
            let store = Arc::new(Mutex::new(s));
            let setup = Setup {
                store: store.clone(),
                filter: core(),
                adult: AdultSources(1 << sukebei.get()),
                switches: flags(off != Off::ShowAdult, off == Off::KillSwitch),
                source: enr,
                background: true,
            };
            let wire = Arc::new(Wire::default());
            let state = Arc::new(StoreState::new(store.clone(), enr));
            let r = start(plugin.clone(), &setup, wire.clone(), state);
            for &i in &ids {
                assert!(r.interest.opened(i));
            }
            // Every opened item is refused; with Show adult content on, the
            // background offers them again and is refused again.
            let want = ids.len() as u64 + if off == Off::ShowAdult { 0 } else { 6 };
            until("the refusals", || n(&r.stats.ineligible) >= want).await;
            let stats = r.stats.clone();
            let mut found = r.stop().await;
            assert_eq!(wire.count(), 0, "{id} {off:?}: {:?}", wire.requests());
            assert_eq!(n(&stats.looked_up), 0, "{id} {off:?}");
            assert!(found.try_recv().is_err(), "{id} {off:?}: an answer emitted");
            let p = store.lock().unwrap().purge_preview(enr).unwrap();
            assert_eq!((p.claims_deleted, p.state_deleted), (0, 0), "{id} {off:?}");
        }
    }
}

// ---- 2. never sent -----------------------------------------------------------

/// Brief §2 (§20.5, §18.7): with every switch on, an item the core holds
/// (a floor hash, a pattern in its name), one a rule hides, drops or
/// quarantines, one stored hidden, a `private_source` one and a private
/// (BEP 27) one are never sent. Only the clean control is, and its request
/// carries its name alone: no hash of any item, and no other item's title.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_flagged_and_private_items_are_never_sent() {
    for plugin in plugins() {
        let id = plugin.manifest().id;
        let (mut s, dht, _, enr) = store_for(id);
        let filter = core();
        rules(
            &filter,
            &[
                ("hidex", Action::Hide),
                ("dropx", Action::Drop),
                ("quarx", Action::Quarantine),
            ],
        );
        let blocked = [
            ("Floorkey Studio Scene 1080p", 0),
            ("Rolea Lexa Studio Scene", 0),
            ("Hidex Studio Scene 1080p", 0),
            ("Dropx Studio Scene 1080p", 0),
            ("Quarx Studio Scene 1080p", 0),
            ("Storedhidden Studio Scene", flags::HIDDEN),
            ("Privatefind Studio Scene", flags::PRIVATE_SOURCE),
            ("Bepprivate Studio Scene", flags::PRIVATE),
        ];
        let mut keys = Vec::new();
        let mut ids = Vec::new();
        for (i, (name, extra)) in blocked.iter().enumerate() {
            let (tid, key) = put(&mut s, name, dht, *extra);
            if i == 0 {
                filter.update(|c| c.cs.add_hash(key.0, Origin::Imported));
            }
            ids.push(tid);
            keys.push(key);
        }
        let (control, control_key) = put(&mut s, MATCHED, dht, 0);
        keys.push(control_key);
        let setup = Setup {
            store: Arc::new(Mutex::new(s)),
            filter,
            adult: AdultSources::default(),
            switches: flags(true, false),
            source: enr,
            background: false,
        };
        let wire = Arc::new(Wire::default());
        let r = start(plugin, &setup, wire.clone(), Arc::new(MemoryState::new()));
        for &i in ids.iter().chain([&control]) {
            assert!(r.interest.opened(i));
        }
        let all = ids.len() as u64 + 1;
        until("every item", || n(&r.stats.done) >= all).await;
        let stats = r.stats.clone();
        r.stop().await;
        assert_eq!(n(&stats.looked_up), 1, "{id}: the control only");
        let seen = wire.requests();
        assert_eq!(seen.len(), 1, "{id}: {seen:?}");
        let text = text_of(&seen[0]);
        assert!(
            text.contains("synthetic"),
            "{id}: the control's name: {text}"
        );
        for key in &keys {
            for k in spellings(key) {
                assert!(!text.contains(&k.to_lowercase()), "{id}: a hash in {text}");
            }
        }
        for (name, _) in blocked {
            let word = name.split(' ').next().unwrap().to_lowercase();
            assert!(!text.contains(&word), "{id}: {word} sent: {text}");
        }
    }
}

// ---- 3. never stored ---------------------------------------------------------

/// ThePornDB's recorded match for [`MATCHED`], run through the runtime:
/// the discovery it emits, and the torrent's id and key.
async fn recorded_match(
    store: Arc<Mutex<Store>>,
    enr: SourceId,
    state: Arc<dyn PluginState>,
) -> Discovery {
    recorded_match_during(store, enr, state, None).await
}

/// [`recorded_match`], running `during` while the lookup is in flight.
async fn recorded_match_during(
    store: Arc<Mutex<Store>>,
    enr: SourceId,
    state: Arc<dyn PluginState>,
    during: Option<Box<dyn Fn() + Send + Sync>>,
) -> Discovery {
    let replay = Replayer::load(&fixture("tpdb_match")).unwrap();
    let wire = Arc::new(Wire {
        replay: Some(replay),
        during,
        ..Wire::default()
    });
    let id = store
        .lock()
        .unwrap()
        .find(&key_of(MATCHED))
        .unwrap()
        .unwrap();
    let setup = Setup {
        store,
        filter: bare(),
        adult: AdultSources::default(),
        switches: flags(true, false),
        source: enr,
        background: false,
    };
    let plugin: Arc<dyn AdultEnricher> = plugins()[0].clone();
    let mut r = start(plugin, &setup, wire.clone(), state);
    assert!(r.interest.opened(id));
    let mut got = None;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while got.is_none() {
        got = tokio::time::timeout_at(deadline, r.found.recv())
            .await
            .expect("the match emitted");
    }
    until("the match counted", || n(&r.stats.matched) == 1).await;
    r.stop().await;
    got.unwrap()
}

fn fixture(case: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/tpdb")
        .join(case)
}

fn key_of(name: &str) -> InfoHash {
    Info::parse(&info_of(name))
        .unwrap()
        .swarm_hashes()
        .next()
        .unwrap()
}

/// The writer's side of a match: SRC-2's filter-before-storage path, with
/// the plugin's source listed as an enrichment source, as the host lists
/// it (`PipelineConfig::adult_enrichers`), and its consent judged again,
/// as the writer judges it.
fn ingest(s: &mut Store, filter: &Handle, d: &Discovery) -> unverified::Ingested {
    let enrichers = EnrichSources::listed(1 << d.source.get());
    let plugin = s
        .sources()
        .unwrap()
        .into_iter()
        .find(|r| r.id == d.source)
        .unwrap()
        .plugin;
    let plugin = plugins()
        .into_iter()
        .find(|p| p.manifest().id == plugin)
        .unwrap();
    let consent = EnricherConsent::of(
        [(d.source, plugin.manifest(), plugin.policy().credentials)],
        enrichers,
    );
    let ctx = Ctx {
        enrichers,
        filter: Some(filter),
        consent: Some(&consent),
        ..Ctx::default()
    };
    let engine = filter.current();
    let stats = UnverifiedStats::default();
    unverified::ingest(
        s,
        &engine,
        ctx,
        std::slice::from_ref(d),
        &stats,
        T,
        &|_, _| {},
    )
    .unwrap()
}

/// Brief §3 (§20.5, plan Q2): the real plugin's answer for an item that the
/// floor holds after its lookup, or whose answer text completes a core
/// pattern with its name, is dropped before storage. No claim from the
/// plugin is stored, the item stays held, and its kind isn't pinned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_for_a_blocked_item_is_dropped_before_storage() {
    // Held on the floor between the lookup and the write.
    let (mut s, dht, _, enr) = store_for(tpdb::ID);
    let (id, key) = put(&mut s, MATCHED, dht, 0);
    let store = Arc::new(Mutex::new(s));
    let state = Arc::new(MemoryState::new());
    let d = recorded_match(store.clone(), enr, state).await;
    let filter = bare();
    filter.update(|c| c.cs.add_hash(key.0, Origin::Imported));
    {
        let mut s = store.lock().unwrap();
        let done = ingest(&mut s, &filter, &d);
        assert!(done.matched.is_empty(), "{done:?}");
        assert!(s.torrent_claims(id).unwrap().is_empty());
        // The floor's hold reaches the stored torrent too: it's removed,
        // not enriched.
        assert_eq!(done.deleted, vec![id], "{done:?}");
        assert_eq!(s.find(&key).unwrap(), None);
    }

    // The answer's performer completes a pattern with the name's term.
    let (mut s, dht, _, enr) = store_for(tpdb::ID);
    let (id, key) = put(&mut s, MATCHED, dht, 0);
    let store = Arc::new(Mutex::new(s));
    let d = recorded_match(store.clone(), enr, Arc::new(MemoryState::new())).await;
    let mut s = store.lock().unwrap();
    let done = ingest(&mut s, &core(), &d);
    assert!(done.matched.is_empty(), "{done:?}");
    assert_eq!(done.deleted, vec![id], "held and removed");
    assert_eq!(s.held_among(&[key.0]).unwrap(), vec![true]);
    assert!(s.torrent_claims(id).unwrap().is_empty());
    assert_eq!(s.find(&key).unwrap(), None);
}

/// Brief §3, the log (§12.1, §18.9): with every pipeline event on, the
/// runtime, the real plugin and the writer log no name, query, answer
/// text or hash, at any kind but the opt-ins. This binary's only sink.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_from_a_lookup_reaches_the_log() {
    struct Capture(Mutex<Vec<String>>);
    impl lodestar_log::Sink for Capture {
        fn record(&self, r: &lodestar_log::Record<'_>) {
            let all = lodestar_log::Mask(u32::MAX).without_opt_ins();
            self.0.lock().unwrap().push(r.text(all, None));
        }
    }
    static CAPTURE: OnceLock<Capture> = OnceLock::new();
    let capture = CAPTURE.get_or_init(|| Capture(Mutex::new(Vec::new())));
    lodestar_log::set_sink(capture).expect("this binary's only sink");
    for c in [
        &lodestar_pipeline::log_events::CATALOGUE,
        &lodestar_filter::log_events::CATALOGUE,
    ] {
        for e in c.events {
            e.set_on(true);
        }
    }
    let (mut s, dht, _, enr) = store_for(tpdb::ID);
    let (id, key) = put(&mut s, MATCHED, dht, 0);
    let store = Arc::new(Mutex::new(s));
    let d = recorded_match(store.clone(), enr, Arc::new(MemoryState::new())).await;
    let done = ingest(&mut store.lock().unwrap(), &core(), &d);
    assert_eq!(done.deleted, vec![id], "the held path ran too");
    // Today the lookup, the plugin and the hold log nothing at all. A canary
    // proves the sink is live, so a line someone adds later is scanned.
    lodestar_log::log_event!(
        lodestar_pipeline::log_events::SOURCE_EGRESS_UP,
        plugin = "canary"
    );
    let lines = capture.0.lock().unwrap().clone();
    assert!(
        lines.iter().any(|l| l.contains("canary")),
        "the sink is live: {lines:?}"
    );
    let mut words: Vec<String> = ["synthetic", "example performer", "example studio"]
        .map(String::from)
        .to_vec();
    words.extend(spellings(&key).into_iter().map(|k| k.to_lowercase()));
    for line in &lines {
        let l = line.to_lowercase();
        for w in &words {
            assert!(!l.contains(w), "{w} logged: {line}");
        }
    }
}

// ---- 4. consent ----------------------------------------------------------------

/// Brief §4 (§18.7): both adult plugins are Account class and in the
/// catalogue the consent gate's own tests walk (SRC-G,
/// `safety.rs::no_enable_path_opens_a_consent_class_in_any_shipped_plugin`,
/// and the API, import and restart paths it lists), and each declares its
/// API key as a required credential: enabling needs it.
#[test]
fn both_adult_plugins_are_account_class_in_the_consent_gate() {
    for plugin in plugins() {
        let m = plugin.manifest();
        assert_eq!(m.consent, Consent::Account, "{}", m.id);
        assert!(!m.always_adult && !m.enabled_by_default, "{}", m.id);
        assert!(catalogue::manifests().any(|c| c.id == m.id), "{}", m.id);
        let creds = plugin.policy().credentials;
        assert!(creds.iter().any(|c| c.name == "api_key" && c.required));
    }
    assert!(Tpdb::need(&[]).is_some() && StashDb::need(&[]).is_some());
    let stored = ["api_key".to_owned()];
    assert!(Tpdb::need(&stored).is_none() && StashDb::need(&stored).is_none());
    assert_eq!((tpdb::ID, stashdb::ID), ("tpdb", "stashdb"));
}

/// Brief §4: withdrawing consent stops lookups at once, before the next
/// one, and so does switching the plugin off, though the plugin itself
/// could still read a key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn withdrawing_consent_or_switching_off_stops_lookups_at_once() {
    for plugin in plugins() {
        let id = plugin.manifest().id;
        let (mut s, dht, _, enr) = store_for(id);
        let items: Vec<i64> = ["First", "Second", "Third"]
            .iter()
            .map(|w| put(&mut s, &format!("Studiox {w} Scene XXX 1080p"), dht, 0).0)
            .collect();
        let store = Arc::new(Mutex::new(s));
        let setup = Setup {
            store: store.clone(),
            filter: core(),
            adult: AdultSources::default(),
            switches: flags(true, false),
            source: enr,
            background: false,
        };
        let wire = Arc::new(Wire::default());
        let r = start(plugin, &setup, wire.clone(), Arc::new(MemoryState::new()));
        assert!(r.interest.opened(items[0]));
        until("the first lookup", || n(&r.stats.done) >= 1).await;
        assert_eq!(wire.count(), 1, "{id}");
        consent(&mut store.lock().unwrap(), enr, false);
        assert!(r.interest.opened(items[1]));
        until("the second item", || n(&r.stats.done) >= 2).await;
        consent(&mut store.lock().unwrap(), enr, true);
        store
            .lock()
            .unwrap()
            .set_source_state(enr, SourceState::Disabled)
            .unwrap();
        assert!(r.interest.opened(items[2]));
        until("the third item", || n(&r.stats.done) >= 3).await;
        let stats = r.stats.clone();
        r.stop().await;
        assert_eq!(wire.count(), 1, "{id}: {:?}", wire.requests());
        assert_eq!(n(&stats.looked_up), 1, "{id}");
    }
}

/// Brief §3 and §4: an answer looked up while consent held, and written
/// after it was withdrawn (the key removed, the source still enabled), is
/// refused at the write: nothing of it is stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_written_after_consent_is_withdrawn_is_not_stored() {
    let (mut s, dht, _, enr) = store_for(tpdb::ID);
    let (id, _) = put(&mut s, MATCHED, dht, 0);
    let store = Arc::new(Mutex::new(s));
    let d = recorded_match(store.clone(), enr, Arc::new(MemoryState::new())).await;
    let mut s = store.lock().unwrap();
    consent(&mut s, enr, false);
    let done = ingest(&mut s, &bare(), &d);
    assert!(done.matched.is_empty(), "{done:?}");
    assert!(s.torrent_claims(id).unwrap().is_empty());
    // Consent given again: the same answer is stored, so the refusal above
    // was the consent's.
    consent(&mut s, enr, true);
    assert_eq!(ingest(&mut s, &bare(), &d).matched, vec![id]);
}

/// A match whose consent is withdrawn while it's looked up isn't cached:
/// the writer refuses it, so once consent is back the item is asked
/// again, not a hit's lifetime later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_match_whose_consent_went_meanwhile_is_not_cached() {
    let (mut s, dht, _, enr) = store_for(tpdb::ID);
    put(&mut s, MATCHED, dht, 0);
    let store = Arc::new(Mutex::new(s));
    let state: Arc<dyn PluginState> = Arc::new(MemoryState::new());
    let during = {
        let store = store.clone();
        Box::new(move || consent(&mut store.lock().unwrap(), enr, false))
    };
    recorded_match_during(store.clone(), enr, state.clone(), Some(during)).await;
    consent(&mut store.lock().unwrap(), enr, true);
    // Asked again with the same plugin state: a cached hit would be skipped
    // and nothing emitted.
    recorded_match(store, enr, state).await;
}

// ---- 6. purge --------------------------------------------------------------------

/// Brief §6 (§18.1 purge, SRC-15b): purging ThePornDB removes the claim its
/// match stored and its cache in `source_state`, keeps the torrent (it
/// didn't find it), and the torrent's classification is derived again
/// without the match: the kind the match pinned is gone from the record,
/// and the torrent is logged for the search index. No rollup moves: a
/// match never changes whether an item is adult (`enrich.rs`), the purge
/// keeps the torrent, and no Insights counter keys on the adult kind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purging_the_source_removes_its_enrichment_and_reconciles_the_record() {
    let (mut s, dht, _, enr) = store_for(tpdb::ID);
    let (id, key) = put(&mut s, MATCHED, dht, 0);
    // Adult by your correction, so the match pins its kind.
    s.set_correction(
        id,
        lodestar_store::ClassCorrection {
            adult: Some(true),
            not_junk: false,
        },
    )
    .unwrap();
    let kind = |s: &Store| {
        let e = s.enrichment(id).unwrap().unwrap();
        (e.version, Record::decode(&e.data).unwrap().adult_kind)
    };
    let before = kind(&s).1;
    let store = Arc::new(Mutex::new(s));
    let state = Arc::new(StoreState::new(store.clone(), enr));
    let mut d = recorded_match(store.clone(), enr, state).await;
    // Detection already reads this name as a scene: the answer is made to
    // say VR, so what the match pinned can be told apart.
    let section = d.found.claims.as_mut().and_then(|c| c.adult.as_mut());
    section.expect("a section").kind = Some(lodestar_sources::claims::AdultKind::Vr);
    let filter = core();
    let mut s = store.lock().unwrap();
    // No core terms this time: the match is stored.
    let clean = bare();
    assert_eq!(ingest(&mut s, &clean, &d).matched, vec![id]);
    assert_eq!(s.torrent_claims(id).unwrap().len(), 1);
    let (_, pinned) = kind(&s);
    assert_eq!(pinned, Some(AdultKind::Vr));
    assert_ne!(before, pinned, "the match changed the record");
    let p = s.purge_preview(enr).unwrap();
    assert_eq!(p.claims_deleted, 1);
    assert!(
        p.state_deleted >= 1,
        "the runtime's cache and budget: {p:?}"
    );

    // Disable, then purge step by step, as the job does.
    s.set_source_state(enr, SourceState::Disabled).unwrap();
    let mut from = Some(PurgeCursor::START);
    while let Some(at) = from {
        from = s.purge_source(enr, at, 2).unwrap().next;
    }
    assert!(s.torrent_claims(id).unwrap().is_empty());
    let p = s.purge_preview(enr).unwrap();
    assert_eq!((p.claims_deleted, p.state_deleted), (0, 0));
    assert_eq!(s.find(&key).unwrap(), Some(id), "the torrent stays");
    // The record is queued to be derived again, and the backfill drops
    // the kind the purged match pinned.
    let (version, _) = kind(&s);
    assert_ne!(version, ENRICHMENT_VERSION, "queued again");
    let ctx = Backfill {
        filter: Some(&filter),
        ..Backfill::default()
    };
    let head = s.reindex_head().unwrap();
    while enrich_stored_with(&mut s, 0, 100, &ctx).unwrap().is_some() {}
    let logged = s.reindex_after(head, 100).unwrap();
    assert!(
        logged.iter().any(|&(_, t)| t == id),
        "reindexed: {logged:?}"
    );
    let (version, after) = kind(&s);
    assert_eq!(version, ENRICHMENT_VERSION);
    assert_eq!(after, before, "the match's kind is gone");
    let e = Record::decode(&s.enrichment(id).unwrap().unwrap().data).unwrap();
    assert_eq!(e.content_type, ContentType::Adult, "still adult by you");
}

// ---- 7. the kill switch ------------------------------------------------------------

/// The resolver for the egress tests: counts lookups, answers none.
#[derive(Default)]
struct NoDns(AtomicUsize);

impl lodestar_integrations::net::Resolve for NoDns {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, std::io::Result<Vec<std::net::IpAddr>>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(std::io::Error::other("no lookups in this test")) })
    }
}

struct Enabled;

impl EgressCredentials for Enabled {
    fn socks5<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<
        'a,
        Result<Option<lodestar_integrations::SocksAuth>, lodestar_sources::egress::CredentialError>,
    > {
        Box::pin(async { Ok(None) })
    }
    fn source_enabled<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<bool, lodestar_sources::egress::CredentialError>> {
        Box::pin(async { Ok(true) })
    }
}

/// Brief §7 (runner security pre-audit, 2026-11-05; SRC-5): an adult
/// plugin whose route is down for good, or a SOCKS5 route not checked yet
/// whose proxy is gone, sends nothing, even with the host's switch saying
/// the kill switch hasn't tripped. The request is refused by the kill
/// switch before any name lookup or connection to the service.
///
/// The host doesn't run the enrichment runtime yet, so it has no
/// `Switches` of its own; `the_binary_wires_no_adult_enrichment_yet` holds
/// the wiring slice to the rule (decisions file, "Kill switch").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unavailable_route_or_an_unchecked_one_whose_proxy_is_gone_sends_nothing() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = closed.local_addr().unwrap();
    drop(closed);
    for plugin in plugins() {
        let id = plugin.manifest().id;
        let dns = Arc::new(NoDns::default());
        let factory = EgressFactory::with_resolver(Arc::new(Enabled), dns.clone());
        // The plugin's own hosts, as the pipeline gives its client.
        let endpoints = Endpoints::manifest(plugin.manifest().endpoints);
        let down = factory.unavailable(
            id,
            RouteKind::Socks5,
            DownReason::SettingUnreadable,
            endpoints.clone(),
        );
        let socks = Egress::Socks5 {
            proxy,
            dns: SocksDns::Proxy,
        };
        let unchecked = factory.build(id, &socks, endpoints).await;
        assert_eq!(unchecked.status().state, RouteState::Unchecked);
        for egress in [Arc::new(down), Arc::new(unchecked)] {
            let (mut s, dht, _, enr) = store_for(id);
            let item = put(&mut s, MATCHED, dht, 0).0;
            let setup = Setup {
                store: Arc::new(Mutex::new(s)),
                filter: core(),
                adult: AdultSources::default(),
                // The host's switch says up: only the client stops it.
                switches: flags(true, false),
                source: enr,
                background: false,
            };
            let r = start(
                plugin.clone(),
                &setup,
                egress.clone(),
                Arc::new(MemoryState::new()),
            );
            assert!(r.interest.opened(item));
            until("the lookup", || n(&r.stats.done) >= 1).await;
            let stats = r.stats.clone();
            r.stop().await;
            assert_eq!(n(&stats.failed), 1, "{id}: refused, not matched");
            let st = egress.status();
            assert!(matches!(st.state, RouteState::Down(_)), "{id}: {st:?}");
            let c = st.counters;
            assert!(c.kill_switch_refusals >= 1, "{id}: {c:?}");
            assert_eq!((c.answered, c.transport_failures), (0, 0), "{id}: {c:?}");
            assert_eq!(dns.0.load(Ordering::SeqCst), 0, "{id}: a name lookup");
        }
    }
}

/// Brief §7, not yet built: nothing runs the adult-enrichment runtime, so
/// nothing can look an item up, and no host implements its `Switches`.
/// A tripwire on spellings, not a proof: it scans every crate's `src`
/// (skipping the runtime's own `adult_enrich.rs`, comment lines and a
/// file's trailing `#[cfg(test)] mod`) and fails on a line with the
/// literal `AdultEnrichSource::new`, or an `impl … Switches for` line in
/// a file that names `adult_enrich`. A constructor or an impl spelled
/// another way would pass it, so the wiring slice owes the kill-switch
/// requirement whether or not this trips: its own test that
/// `kill_switch_tripped` is true for every route state but Up (Unchecked
/// and Down included), and its Show adult content and drawer-interest
/// paths added to this gate.
#[test]
fn the_binary_wires_no_adult_enrichment_yet() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let own = std::path::Path::new("pipeline/src/adult_enrich.rs");
    let mut stack: Vec<_> = std::fs::read_dir(&crates)
        .unwrap()
        .map(|e| e.unwrap().path().join("src"))
        .filter(|p| p.is_dir())
        .collect();
    let (mut hits, mut files) = (Vec::new(), 0);
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with(own) {
                files += 1;
                // Windows checkouts may carry CRLF endings, which would
                // hide the `#[cfg(test)] mod` split below.
                let text = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("\r\n", "\n");
                // A crate's own unit tests (its `#[cfg(test)] mod`) may
                // stand in for the host.
                let text = text.split("\n#[cfg(test)]\nmod ").next().unwrap();
                let code: Vec<&str> = text
                    .lines()
                    .map(str::trim_start)
                    .filter(|l| !l.starts_with("//"))
                    .collect();
                let names = code.iter().any(|l| l.contains("adult_enrich"));
                for l in &code {
                    let builds = l.contains("AdultEnrichSource::new");
                    let switches = names && l.starts_with("impl") && l.contains("Switches for");
                    if builds || switches {
                        hits.push(format!("{}: {l}", path.display()));
                    }
                }
            }
        }
    }
    assert!(files > 500, "scanned the workspace's sources: {files}");
    assert!(hits.is_empty(), "adult enrichment is wired: {hits:?}");
}
