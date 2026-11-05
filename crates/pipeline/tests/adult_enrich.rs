//! The adult-enrichment runtime (ENR-1, `docs/decisions/2026-11-05-ENR-1.md`):
//! what may be sent to ThePornDB or StashDB, and what becomes of the
//! answer.
//!
//! - no ineligible item is ever handed to a plugin (a property test over
//!   stored items and switches, through the running runtime);
//! - a core-held item with a perfect match stays held;
//! - no request carries the info hash;
//! - the plugin stops after shutdown;
//! - the refile sweep judges enrichment claims again when the rules change;
//! - a verified item from an always-adult source is eligible, an
//!   unverified one isn't;
//! - a match pins the adult kind, and its text is judged before storage.
//!
//! No fixed sleeps: every wait is for a condition, with a 10 s deadline.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::{AdultKind, ContentType, Record};
use lodestar_filter::{
    Action, Core, CsList, Engine, Handle, Lists, Switches as CoreSwitches, TermSet,
};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::adult_enrich::{
    self, AdultEnrichSource, Config, EnrichSources, GateCtx, Ineligible, Interest, STRATEGY,
    Switches,
};
use lodestar_pipeline::enrich::{enrich_with_rule, file_infos};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::unverified::{self, Ctx, Phase, UnverifiedStats};
use lodestar_sources::claims::{AdultEnrichment, AdultKind as Kind2};
use lodestar_sources::enrich::{AdultEnricher, Lookup, Query, QueryForm};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Emitter, Fetch, HostLimiter, HttpError, Kind, Kit, Manifest,
    MemoryState, Network, PluginError, Policy, Priority, Request, Response, Risk, Shutdown,
    SourceCtx, SourcePlugin,
};
use lodestar_store::claims::Claims;
use lodestar_store::unverified::{Network as Net, NewUnverified};
use lodestar_store::{
    ClassCorrection, NewTorrent, SourceId, SourceState, Store, StoreConfig, flags,
};
use proptest::prelude::*;
use tokio::sync::mpsc;

const T: i64 = 1_700_000_000;
const DEADLINE: Duration = Duration::from_secs(10);

static MANIFEST: Manifest = Manifest {
    id: "test-enrich",
    name: "Test enrichment",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    endpoints: &[],
    risk: Risk::Medium,
    consent: Consent::Account,
    default_priority: Priority::Normal,
    enabled_by_default: false,
    always_adult: false,
};

fn info_dict(name: &str) -> Vec<u8> {
    info_with_file(name, &format!("{}.mp4", name.replace(' ', ".")))
}

/// An info dict named `name` holding one file called `file`.
fn info_with_file(name: &str, file: &str) -> Vec<u8> {
    let mut f = BTreeMap::new();
    f.insert(b"length".to_vec(), OwnedValue::Int(900 << 20));
    f.insert(
        b"path".to_vec(),
        OwnedValue::List(vec![OwnedValue::from(file)]),
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
    put_raw(store, name, &info_dict(name), source, extra, adult)
}

fn put_raw(
    store: &mut Store,
    name: &str,
    raw: &[u8],
    source: SourceId,
    extra: i64,
    adult: bool,
) -> (i64, InfoHash) {
    let raw = raw.to_vec();
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

/// A store with the enrichment source registered, enabled and consented
/// to (its Account credential stored), and a DHT source.
fn store() -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let enr = s.register_source(MANIFEST.id, "1", 0).unwrap();
    s.update_source_secrets(
        enr,
        &[("api_key", b"sealed")],
        &[],
        Some(SourceState::Enabled),
    )
    .unwrap();
    (s, dht, enr)
}

/// The core with stand-in term lists: `lexa` is sexual, `rolea` a child
/// role, so "rolea lexa" is a CS pattern hit.
fn stand_in() -> Arc<Handle> {
    Arc::new(Handle::new(Engine::new(core())))
}

fn core() -> Core {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Core::new(lists, CsList::default(), CoreSwitches::default())
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
    });
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

fn on() -> Arc<Flags> {
    let f = Flags::default();
    f.show_adult.store(true, Ordering::SeqCst);
    Arc::new(f)
}

/// A plugin that records every query, sends each as one request through
/// its kit, and answers with `answer`. With `hang`, a lookup never ends;
/// `live` counts lookups in progress (dropped futures included).
struct Recorder {
    form: QueryForm,
    queries: Mutex<Vec<Query>>,
    answer: Option<AdultEnrichment>,
    hang: bool,
    /// Sends each query as a request (one host: the limiter paces it).
    send: bool,
    live: Arc<AtomicUsize>,
}

impl Recorder {
    fn new(answer: Option<AdultEnrichment>) -> Arc<Recorder> {
        Arc::new(Recorder {
            form: QueryForm::Name,
            queries: Mutex::default(),
            answer,
            hang: false,
            send: false,
            live: Arc::default(),
        })
    }

    fn names(&self) -> Vec<String> {
        self.queries
            .lock()
            .unwrap()
            .iter()
            .flat_map(|q| q.text().into_iter().map(String::from))
            .collect()
    }
}

struct Live(Arc<AtomicUsize>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AdultEnricher for Recorder {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn query_form(&self) -> QueryForm {
        self.form
    }

    fn lookup<'a>(
        &'a self,
        kit: &'a Kit,
        query: &'a Query,
    ) -> BoxFuture<'a, Result<Lookup, PluginError>> {
        Box::pin(async move {
            self.live.fetch_add(1, Ordering::SeqCst);
            let _live = Live(self.live.clone());
            self.queries.lock().unwrap().push(query.clone());
            if self.send {
                let q = query.text().join(" ");
                let url = format!(
                    "https://api.example.test/scenes?parse={}",
                    q.replace(' ', "+")
                );
                let _ = kit.http.send(&Request::get(&url)).await;
            }
            if self.hang {
                std::future::pending::<()>().await;
            }
            Ok(match &self.answer {
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
    stats: Arc<adult_enrich::EnrichStats>,
    wire: Arc<Wire>,
}

fn run(
    plugin: Arc<Recorder>,
    store: Arc<Mutex<Store>>,
    filter: Arc<Handle>,
    adult: AdultSources,
    switches: Arc<Flags>,
    source: SourceId,
    config: Config,
) -> Running {
    let (src, interest) = AdultEnrichSource::new(plugin, store, filter, adult, switches, config);
    let stats = src.stats();
    let (tx, found) = mpsc::channel(64);
    let (stop, shutdown) = Shutdown::new();
    let wire = Arc::new(Wire::default());
    let kit = Kit::with_limiter(
        Arc::new(MemoryState::new()),
        wire.clone(),
        HostLimiter::new(Policy::STRICT.limits(&Default::default())),
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

/// Fast pacing, no background: only what the test asks for is looked up.
fn interest_only() -> Config {
    Config {
        per_day: 0,
        interval: Duration::ZERO,
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
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn section(kind: Option<Kind2>, title: &str) -> AdultEnrichment {
    AdultEnrichment {
        kind,
        title: Some(title.into()),
        studio: Some("Studiox".into()),
        performers: vec!["Ann Example".into()],
        ..AdultEnrichment::new("3f2c9a1e-0001")
    }
}

/// The facts one generated item is stored with.
#[derive(Debug, Clone)]
struct Spec {
    adult: bool,
    /// Found by an always-adult source (stored with detection's type).
    masked: bool,
    hidden: bool,
    private: bool,
    bep27: bool,
    floor: bool,
    not_adult: bool,
    unverified: bool,
    partial: bool,
    /// A rule of yours matches it now: hide, drop or quarantine.
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
        proptest::collection::vec(proptest::bool::weighted(0.2), 9),
        rule,
    )
        .prop_map(|(b, rule)| Spec {
            adult: !b[0],
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

fn rule_word(a: Action) -> &'static str {
    match a {
        Action::Hide => "Hidex",
        Action::Drop => "Dropx",
        _ => "Quarx",
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// Whatever is stored and whatever the switches say, the plugin is
    /// handed exactly the eligible items, through the running runtime.
    #[test]
    fn no_ineligible_item_is_ever_handed_to_a_plugin(
        specs in proptest::collection::vec(spec(), 1..7),
        show_adult in proptest::bool::weighted(0.8),
        tripped in proptest::bool::weighted(0.2),
        enabled in proptest::bool::weighted(0.8),
        consented in proptest::bool::weighted(0.8),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let (mut s, dht, enr) = store();
            let sukebei = s.register_source("sukebei", "1", 0).unwrap();
            let mask = AdultSources(1 << sukebei.get());
            if !consented {
                s.update_source_secrets(enr, &[], &["api_key"], None).unwrap();
            }
            if !enabled {
                s.set_source_state(enr, SourceState::Disabled).unwrap();
            }
            let filter = stand_in();
            let mut ids = Vec::new();
            let mut want = Vec::new();
            for (i, sp) in specs.iter().enumerate() {
                let word = sp.rule.map_or("Plain", rule_word);
                let name = format!("Item{i} Scene {word}");
                if sp.unverified {
                    // An unverified item's id: never a torrent's.
                    let key = [i as u8 + 100; 20];
                    let c = Claims { name: Some(name.clone()), ..Claims::default() };
                    let put = s.put_unverified(&[NewUnverified {
                        network: Net::Bt, key: &key, source: sukebei, private_source: false,
                        claims: Some(&c), at: T,
                    }]).unwrap();
                    let lodestar_store::unverified::Put::New(id) = put[0] else { panic!("new") };
                    // Items take their ids from the torrents' sequence, so
                    // this is a real id an opened drawer could send, held
                    // by an item and by no torrent.
                    assert!(s.torrent(id).unwrap().is_none());
                    assert_eq!(s.unverified(Net::Bt, &key).unwrap().map(|i| i.id), Some(id));
                    ids.push(id);
                    continue;
                }
                let extra = if sp.hidden { flags::HIDDEN } else { 0 }
                    | if sp.private { flags::PRIVATE_SOURCE } else { 0 }
                    | if sp.bep27 { flags::PRIVATE } else { 0 }
                    | if sp.partial { flags::FILES_PARTIAL | flags::IMPORTED } else { 0 };
                let source = if sp.masked { sukebei } else { dht };
                let (id, key) = put(&mut s, &name, source, extra, sp.adult);
                if sp.not_adult {
                    s.set_correction(id, ClassCorrection { adult: Some(false), not_junk: false }).unwrap();
                    let e = enrich_with_rule(&name, &[], Some(false));
                    s.set_enrichments(&[(id, e)]).unwrap();
                }
                if sp.floor {
                    // A hash on the floor's list: CS-1 holds it.
                    filter.update(|core| core.cs.add_hash(key.0, lodestar_filter::Origin::Imported));
                }
                ids.push(id);
                let adult = !sp.not_adult && (sp.adult || sp.masked);
                let ok = show_adult && !tripped && enabled && consented && adult
                    && !sp.hidden && !sp.private && !sp.bep27 && !sp.partial
                    && !sp.floor && sp.rule.is_none();
                if ok {
                    want.push(name);
                }
            }
            let rows = [Action::Hide, Action::Drop, Action::Quarantine]
                .iter()
                .enumerate()
                .map(|(n, &a)| lodestar_store::RuleRow {
                    id: n as i64 + 1,
                    query: rule_word(a).to_lowercase(),
                    action: a.code(),
                    enabled: true,
                    shadow_until: None,
                    unless_query: None,
                })
                .collect();
            filter.update_engine(|e| {
                let notes = e.load_rules(&lodestar_store::RuleSet { rules: rows, ..Default::default() });
                assert!(notes.is_empty(), "{notes:?}");
            });
            let flags = Flags::default();
            flags.show_adult.store(show_adult, Ordering::SeqCst);
            flags.tripped.store(tripped, Ordering::SeqCst);
            let plugin = Recorder::new(None);
            let r = run(plugin.clone(), Arc::new(Mutex::new(s)), filter, mask,
                Arc::new(flags), enr, interest_only());
            for &id in &ids {
                assert!(r.interest.opened(id));
            }
            let n = ids.len() as u64;
            until("every item", || r.stats.done.load(Ordering::SeqCst) >= n).await;
            let _ = r.stop.send(true);
            let _ = r.task.await;
            let mut got = plugin.names();
            got.sort();
            want.sort();
            assert_eq!(got, want);
        });
    }
}

/// A swarm hash with a tombstone or quarantine row is never looked up,
/// even with the torrent still stored under its other hash.
#[test]
fn a_held_swarm_hash_is_never_looked_up() {
    let engine = stand_in().current();
    let flags = on();
    for table in ["tombstones", "quarantine"] {
        let (mut s, dht, enr) = store();
        let (id, key) = put(&mut s, "Garden Scene", dht, 0, true);
        let ctx = GateCtx {
            manifest: &MANIFEST,
            source: enr,
            form: QueryForm::Name,
            adult: AdultSources::default(),
            switches: &*flags,
        };
        assert!(
            adult_enrich::check(&mut s, &engine, ctx, id, T)
                .unwrap()
                .is_ok()
        );
        let sql = match table {
            "tombstones" => "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'CS-2a', 0)",
            _ => {
                "INSERT INTO quarantine (hash, rule, sealed, at, expires_at) VALUES (?1, 'CS-2a', NULL, 0, 9)"
            }
        };
        s.connection().execute(sql, [&key.0[..]]).unwrap();
        let got = adult_enrich::check(&mut s, &engine, ctx, id, T).unwrap();
        assert!(
            matches!(got, Err(Ineligible::Held | Ineligible::NotStored)),
            "{table}: {got:?}"
        );
        if s.torrent(id).unwrap().is_some() {
            assert_eq!(got, Err(Ineligible::Held), "{table}");
        }
    }
}

/// The parsed form with no title to send: nothing is sent.
#[test]
fn no_title_means_no_query() {
    let (mut s, dht, enr) = store();
    let (id, _) = put(&mut s, "1080p", dht, 0, true);
    let engine = stand_in().current();
    let flags = on();
    let ctx = GateCtx {
        manifest: &MANIFEST,
        source: enr,
        form: QueryForm::Parsed,
        adult: AdultSources::default(),
        switches: &*flags,
    };
    let got = adult_enrich::check(&mut s, &engine, ctx, id, T).unwrap();
    assert!(
        matches!(&got, Err(Ineligible::NoQuery))
            || matches!(&got, Ok(c) if !c.query.text().iter().any(|t| t.contains("1080p"))),
        "{got:?}"
    );
}

/// The gate's reasons for an always-adult source's items: a verified one
/// is eligible through the mask alone; an unverified one never is.
#[test]
fn an_always_adult_item_is_eligible_only_once_verified() {
    let (mut s, _dht, enr) = store();
    let nyaa = s.register_source("sukebei", "1", 0).unwrap();
    let adult = AdultSources(1 << nyaa.get());
    // Detection's own type: not adult. Only the mask makes it so.
    let (id, key) = put(&mut s, "Garden Tour 2022", nyaa, 0, false);
    let c = Claims {
        name: Some("Garden Tour 2023".into()),
        ..Claims::default()
    };
    let put = s
        .put_unverified(&[NewUnverified {
            network: Net::Bt,
            key: &[9; 20],
            source: nyaa,
            private_source: false,
            claims: Some(&c),
            at: T,
        }])
        .unwrap();
    let lodestar_store::unverified::Put::New(unverified) = put[0] else {
        panic!("new")
    };
    let engine = stand_in().current();
    let flags = on();
    let ctx = GateCtx {
        manifest: &MANIFEST,
        source: enr,
        form: QueryForm::Name,
        adult,
        switches: &*flags,
    };
    let got = adult_enrich::check(&mut s, &engine, ctx, id, T)
        .unwrap()
        .unwrap();
    assert_eq!(got.key, key);
    assert_eq!(got.query, Query::name("Garden Tour 2022").unwrap());
    assert_eq!(
        adult_enrich::check(&mut s, &engine, ctx, unverified, T).unwrap(),
        Err(Ineligible::NotStored)
    );
    // Without the mask, the same verified item isn't adult.
    let plain = GateCtx {
        adult: AdultSources::default(),
        ..ctx
    };
    assert_eq!(
        adult_enrich::check(&mut s, &engine, plain, id, T).unwrap(),
        Err(Ineligible::NotAdult)
    );
    // Your Not adult correction wins over the mask.
    s.set_correction(
        id,
        ClassCorrection {
            adult: Some(false),
            not_junk: false,
        },
    )
    .unwrap();
    assert_eq!(
        adult_enrich::check(&mut s, &engine, ctx, id, T).unwrap(),
        Err(Ineligible::NotAdult)
    );
}

/// The parsed form sends the parser's title and date, nothing else.
#[test]
fn the_parsed_form_sends_only_the_parsers_fields() {
    let (mut s, dht, enr) = store();
    let (id, _) = put(&mut s, "Studiox 2024.03.05 Garden Walk 1080p", dht, 0, true);
    let engine = stand_in().current();
    let flags = on();
    let ctx = GateCtx {
        manifest: &MANIFEST,
        source: enr,
        form: QueryForm::Parsed,
        adult: AdultSources::default(),
        switches: &*flags,
    };
    let got = adult_enrich::check(&mut s, &engine, ctx, id, T)
        .unwrap()
        .unwrap();
    let Query::Parsed { date, title, .. } = &got.query else {
        panic!("parsed: {:?}", got.query)
    };
    assert!(!title.contains("1080p"), "only the title");
    assert_eq!(date.map(|d| d.to_string()), Some("2024-03-05".into()));
}

/// No request a plugin sends carries the info hash in any form: the
/// query has nowhere to put it.
#[tokio::test]
async fn no_request_carries_the_info_hash() {
    let (mut s, dht, enr) = store();
    let (id, key) = put(&mut s, "Garden Scene", dht, 0, true);
    let plugin = Arc::new(Recorder {
        send: true,
        ..Arc::into_inner(Recorder::new(Some(section(Some(Kind2::Scene), "Garden")))).unwrap()
    });
    let mut r = run(
        plugin.clone(),
        Arc::new(Mutex::new(s)),
        stand_in(),
        AdultSources::default(),
        on(),
        enr,
        interest_only(),
    );
    assert!(r.interest.opened(id));
    let d = tokio::time::timeout(DEADLINE, r.found.recv())
        .await
        .expect("a match")
        .unwrap();
    // The answer is stored under the key: that never left the install.
    assert_eq!(d.found.key, key);
    assert_eq!(d.found.strategy, STRATEGY);
    assert!(d.found.peers.is_empty() && !d.found.private_source);
    let hex: String = key.0.iter().map(|b| format!("{b:02x}")).collect();
    let requests = r.wire.0.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    for req in &requests {
        let mut bytes = format!("{req:?}").into_bytes();
        bytes.extend(req.url.as_bytes());
        for (n, v) in &req.headers {
            bytes.extend(n.as_bytes());
            bytes.extend(v.as_bytes());
        }
        bytes.extend(&req.body);
        let text = String::from_utf8_lossy(&bytes).to_lowercase();
        assert!(!text.contains(&hex), "hex hash sent");
        assert!(!bytes.windows(20).any(|w| w == key.0), "raw hash sent");
    }
    let _ = r.stop.send(true);
    r.task.await.unwrap().unwrap();
}

/// After shutdown the plugin stops: a lookup in progress is dropped and
/// the run ends.
#[tokio::test]
async fn the_plugin_stops_after_shutdown() {
    let (mut s, dht, enr) = store();
    let (id, _) = put(&mut s, "Garden Scene", dht, 0, true);
    let plugin = Arc::new(Recorder {
        hang: true,
        ..Arc::into_inner(Recorder::new(None)).unwrap()
    });
    let live = plugin.live.clone();
    let r = run(
        plugin,
        Arc::new(Mutex::new(s)),
        stand_in(),
        AdultSources::default(),
        on(),
        enr,
        interest_only(),
    );
    assert!(r.interest.opened(id));
    until("the lookup to start", || live.load(Ordering::SeqCst) == 1).await;
    r.stop.send(true).unwrap();
    tokio::time::timeout(DEADLINE, r.task)
        .await
        .expect("stopped within the deadline")
        .unwrap()
        .unwrap();
    until("the lookup to be dropped", || {
        live.load(Ordering::SeqCst) == 0
    })
    .await;
}

/// The writer's side: what one emitted match becomes, through SRC-2's
/// filter-before-storage path.
fn ingest(s: &mut Store, engine: &Engine, d: &Discovery) -> unverified::Ingested {
    // The discovery's source is an enrichment source, as the runtime's is.
    let ctx = Ctx {
        enrichers: EnrichSources(1 << d.source.get()),
        ..Ctx::default()
    };
    ingest_with(s, engine, ctx, d)
}

/// [`ingest`] with `ctx`, under a live filter holding `engine`.
fn ingest_with(
    s: &mut Store,
    engine: &Engine,
    ctx: Ctx<'_>,
    d: &Discovery,
) -> unverified::Ingested {
    let stats = UnverifiedStats::default();
    let filter = Handle::new(engine.clone());
    let ctx = Ctx {
        filter: ctx.filter.or(Some(&filter)),
        ..ctx
    };
    unverified::ingest(
        s,
        engine,
        ctx,
        std::slice::from_ref(d),
        &stats,
        T,
        &|_, _| {},
    )
    .unwrap()
}

fn matched(source: SourceId, key: InfoHash, a: AdultEnrichment) -> Discovery {
    let claims = lodestar_sources::Claims {
        adult: Some(a),
        ..Default::default()
    };
    Discovery {
        source,
        found: lodestar_sources::Found::key(key, STRATEGY).with_claims(claims),
        at: T,
        reply: false,
    }
}

/// A match pins the adult kind at the match's confidence, through ENR-6's
/// entry point, and the claim is stored on the torrent.
#[test]
fn a_match_pins_the_adult_kind() {
    let (mut s, dht, enr) = store();
    let (id, key) = put(&mut s, "Garden Scene", dht, 0, true);
    // Adult by your Is adult correction, as the store has it.
    s.set_correction(
        id,
        ClassCorrection {
            adult: Some(true),
            not_junk: false,
        },
    )
    .unwrap();
    let before = Record::decode(&s.enrichment(id).unwrap().unwrap().data).unwrap();
    let engine = stand_in().current();
    let done = ingest(
        &mut s,
        &engine,
        &matched(enr, key, section(Some(Kind2::Vr), "Garden")),
    );
    assert_eq!(done.matched, vec![id]);
    assert!(done.touched.is_empty(), "a lookup isn't a sighting");
    let claims = s.torrent_claims(id).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].source, enr);
    let after = Record::decode(&s.enrichment(id).unwrap().unwrap().data).unwrap();
    assert_eq!(after.content_type, ContentType::Adult);
    assert_eq!(after.adult_kind, Some(AdultKind::Vr));
    assert_ne!(before.adult_kind, Some(AdultKind::Vr));
    assert_eq!(
        after.type_confidence, 100,
        "your correction keeps its confidence"
    );

    // Adult by detection: the match's kind, at no less than its 99.
    let (id, key) = put(&mut s, "Studiox Garden Scene XXX 1080p", dht, 0, false);
    let before = Record::decode(&s.enrichment(id).unwrap().unwrap().data).unwrap();
    assert_eq!(before.content_type, ContentType::Adult);
    let done = ingest(
        &mut s,
        &engine,
        &matched(enr, key, section(Some(Kind2::Movie), "Garden")),
    );
    assert_eq!(done.matched, vec![id]);
    let after = Record::decode(&s.enrichment(id).unwrap().unwrap().data).unwrap();
    assert_eq!(after.adult_kind, Some(AdultKind::Movie));
    assert!(after.type_confidence >= 99);
    // A section without a kind pins nothing.
    let (id, key) = put(&mut s, "Studiox Other Scene XXX 1080p", dht, 0, false);
    let before = s.enrichment(id).unwrap().unwrap();
    ingest(&mut s, &engine, &matched(enr, key, section(None, "Other")));
    assert_eq!(s.enrichment(id).unwrap().unwrap(), before);
}

/// A match for a key that isn't a stored torrent, or through a private
/// source, is refused: nothing is stored.
#[test]
fn a_match_needs_a_stored_torrent() {
    let (mut s, _dht, enr) = store();
    let engine = stand_in().current();
    let d = matched(enr, InfoHash([7; 20]), section(None, "Garden"));
    let done = ingest(&mut s, &engine, &d);
    assert!(done.matched.is_empty());
    assert_eq!(s.unverified(Net::Bt, &[7; 20]).unwrap(), None);
}

/// The enrichment text is judged before storage, stricter only: a core
/// pattern across the torrent's name and the scene title holds the hash.
/// And a core-held item with a perfect match stays held.
#[test]
fn a_core_held_item_with_a_perfect_match_stays_held() {
    let (mut s, dht, enr) = store();
    let engine = stand_in().current();
    // The name alone is clean; the scene title adds the child-role term.
    let (id, key) = put(&mut s, "Garden Lexa", dht, 0, true);
    let done = ingest(
        &mut s,
        &engine,
        &matched(enr, key, section(Some(Kind2::Scene), "Rolea")),
    );
    assert!(done.matched.is_empty());
    assert_eq!(done.deleted, vec![id], "held and removed");
    assert_eq!(s.held_among(&[key.0]).unwrap(), vec![true]);
    // The same perfect match again can't release it or store anything.
    let again = ingest(
        &mut s,
        &engine,
        &matched(enr, key, section(Some(Kind2::Scene), "Garden")),
    );
    assert!(again.matched.is_empty());
    assert_eq!(s.held_among(&[key.0]).unwrap(), vec![true]);
    assert_eq!(s.find(&key).unwrap(), None);
    // And the gate never offers it.
    let flags = on();
    let ctx = GateCtx {
        manifest: &MANIFEST,
        source: enr,
        form: QueryForm::Name,
        adult: AdultSources::default(),
        switches: &*flags,
    };
    assert_eq!(
        adult_enrich::check(&mut s, &engine, ctx, id, T).unwrap(),
        Err(Ineligible::NotStored)
    );
}

/// The refile sweep judges stored enrichment claims again when the rules
/// change: a rule matching the studio now takes the claims away, and a
/// core that now matches the title holds the torrent.
#[test]
fn the_refile_sweep_judges_enrichment_claims() {
    let (mut s, dht, enr) = store();
    // Stored under an engine with no term lists: nothing matches yet.
    let none: &[&str] = &[];
    let empty = Lists {
        sexual: TermSet::of(none),
        child_roles: TermSet::of(none),
        codes: TermSet::of(none),
    };
    let filter = Arc::new(Handle::new(Engine::new(Core::new(
        empty,
        CsList::default(),
        CoreSwitches::default(),
    ))));
    let (a, ka) = put(&mut s, "Garden One", dht, 0, true);
    let (b, kb) = put(&mut s, "Garden Two", dht, 0, true);
    let engine = filter.current();
    let mut other = section(Some(Kind2::Scene), "Plain");
    other.studio = Some("Blockedstudio".into());
    ingest(
        &mut s,
        &engine,
        &matched(enr, ka, section(Some(Kind2::Scene), "Rolea Lexa")),
    );
    ingest(&mut s, &engine, &matched(enr, kb, other));
    assert_eq!(s.torrent_claims(a).unwrap().len(), 1);
    assert_eq!(s.torrent_claims(b).unwrap().len(), 1);
    // The rules change: the core's lists, and a rule of yours.
    filter.update_engine(|e| e.core = core());
    rule(&filter, "blockedstudio", Action::Drop);
    let version = filter.current().version;
    let step = unverified::refile_step(
        &mut s,
        &filter,
        version,
        Phase::Claims,
        0,
        (0, 0),
        Ctx::default(),
        T,
        &|_, _| {},
    )
    .unwrap();
    assert!(!step.rejected);
    assert_eq!(step.removed, 2);
    assert_eq!(step.deleted, vec![a], "the core holds the torrent");
    assert_eq!(s.held_among(&[ka.0, kb.0]).unwrap(), vec![true, false]);
    assert!(
        s.torrent_claims(b).unwrap().is_empty(),
        "your rule only drops the claims"
    );
    assert!(s.torrent(b).unwrap().is_some());
}

/// The background backfill stops at the daily cap, and the next item keeps
/// its place for the next day.
#[tokio::test]
async fn the_background_stops_at_the_daily_cap() {
    let (mut s, dht, enr) = store();
    for i in 0..5 {
        put(&mut s, &format!("Garden Scene {i}"), dht, 0, true);
    }
    let plugin = Recorder::new(None);
    let config = Config {
        per_day: 3,
        interval: Duration::ZERO,
        ..Config::default()
    };
    let r = run(
        plugin.clone(),
        Arc::new(Mutex::new(s)),
        stand_in(),
        AdultSources::default(),
        on(),
        enr,
        config,
    );
    until("three lookups", || {
        r.stats.looked_up.load(Ordering::SeqCst) == 3
    })
    .await;
    // The fourth waits on the budget.
    until("the fourth", || r.stats.done.load(Ordering::SeqCst) >= 4).await;
    let _ = r.stop.send(true);
    r.task.await.unwrap().unwrap();
    assert_eq!(plugin.queries.lock().unwrap().len(), 3, "the cap holds");
}

/// A miss is cached for the item, so opening it again doesn't ask again.
#[tokio::test]
async fn a_cached_miss_is_not_asked_again() {
    let (mut s, dht, enr) = store();
    let (id, _) = put(&mut s, "Garden Scene", dht, 0, true);
    let plugin = Recorder::new(None);
    let r = run(
        plugin.clone(),
        Arc::new(Mutex::new(s)),
        stand_in(),
        AdultSources::default(),
        on(),
        enr,
        interest_only(),
    );
    assert!(r.interest.opened(id));
    until("the miss", || r.stats.missed.load(Ordering::SeqCst) == 1).await;
    assert!(r.interest.opened(id));
    until("the cached skip", || {
        r.stats.cached.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(plugin.queries.lock().unwrap().len(), 1);
    let _ = r.stop.send(true);
    r.task.await.unwrap().unwrap();
}

/// Shutdown ends the run while the background is passing over items the
/// gate refuses, before any lookup was made.
#[tokio::test]
async fn shutdown_ends_the_run_while_it_skips_items() {
    let (mut s, dht, enr) = store();
    for i in 0..300 {
        put(
            &mut s,
            &format!("Hidden Scene {i}"),
            dht,
            flags::HIDDEN,
            true,
        );
    }
    let plugin = Recorder::new(None);
    let config = Config {
        interval: Duration::ZERO,
        ..Config::default()
    };
    let r = run(
        plugin.clone(),
        Arc::new(Mutex::new(s)),
        stand_in(),
        AdultSources::default(),
        on(),
        enr,
        config,
    );
    until("a skipped item", || {
        r.stats.ineligible.load(Ordering::SeqCst) >= 1
    })
    .await;
    r.stop.send(true).unwrap();
    tokio::time::timeout(DEADLINE, r.task)
        .await
        .expect("stopped within the deadline")
        .unwrap()
        .unwrap();
    assert!(plugin.queries.lock().unwrap().is_empty());
}

/// The plugin's state can't touch the runtime's caps and caches.
#[tokio::test]
async fn the_plugin_cannot_reach_the_runtimes_state() {
    struct Sneaky(Mutex<Vec<bool>>);
    impl AdultEnricher for Sneaky {
        fn manifest(&self) -> &Manifest {
            &MANIFEST
        }
        fn query_form(&self) -> QueryForm {
            QueryForm::Name
        }
        fn lookup<'a>(
            &'a self,
            kit: &'a Kit,
            _query: &'a Query,
        ) -> BoxFuture<'a, Result<Lookup, PluginError>> {
            Box::pin(async move {
                let tried = [
                    kit.state.set("enr1/budget", vec![0; 12]).await.is_ok(),
                    kit.state.get("enr1/cursor").await.is_ok(),
                    kit.state.set("own/cursor", vec![1]).await.is_ok(),
                ];
                self.0.lock().unwrap().extend(tried);
                Ok(Lookup::NoMatch)
            })
        }
    }
    let (mut s, dht, enr) = store();
    let (id, _) = put(&mut s, "Garden Scene", dht, 0, true);
    let plugin = Arc::new(Sneaky(Mutex::default()));
    let (src, interest) = AdultEnrichSource::new(
        plugin.clone(),
        Arc::new(Mutex::new(s)),
        stand_in(),
        AdultSources::default(),
        on(),
        interest_only(),
    );
    let stats = src.stats();
    let (tx, _found) = mpsc::channel(4);
    let (stop, shutdown) = Shutdown::new();
    let ctx = SourceCtx {
        source: enr,
        emitter: Emitter::new(enr, tx),
        peer_requests: None,
        peer_replies: None,
        shutdown,
        kit: Kit::default(),
    };
    let task = tokio::spawn(async move { src.run(ctx).await });
    assert!(interest.opened(id));
    until("the lookup", || stats.done.load(Ordering::SeqCst) == 1).await;
    assert_eq!(*plugin.0.lock().unwrap(), vec![false, false, true]);
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}

/// The per-host rate is held to one request a second, with no burst.
#[test]
fn the_policy_is_capped_at_one_request_a_second() {
    let mut fast = Policy::STRICT;
    fast.caps.per_host = lodestar_sources::HostRate::per_second(50);
    fast.caps.per_host.burst = 10;
    let p = adult_enrich::capped(fast);
    assert!(p.caps.per_host.interval >= lodestar_sources::enrich::MIN_INTERVAL);
    assert_eq!(p.caps.per_host.burst, 1);
    assert_eq!(
        adult_enrich::capped(Policy::STRICT).caps.per_host.interval,
        Policy::STRICT.caps.per_host.interval
    );
}

/// Q2's "never the files": an uninformative name whose title would come
/// from a file sends nothing in either form, and an informative name sends
/// none of a file's name.
#[test]
fn a_file_name_is_never_sent() {
    let (mut s, dht, enr) = store();
    let raw = info_with_file("New folder", "Secretfile.Garden.Walk.2024.03.05.1080p.mp4");
    let (vague, _) = put_raw(&mut s, "New folder", &raw, dht, 0, true);
    let raw = info_with_file(
        "Studiox 2024.03.05 Garden Walk",
        "Secretfile.Other.Name.mp4",
    );
    let (named, _) = put_raw(&mut s, "Studiox 2024.03.05 Garden Walk", &raw, dht, 0, true);
    let engine = stand_in().current();
    let flags = on();
    let ctx = |form| GateCtx {
        manifest: &MANIFEST,
        source: enr,
        form,
        adult: AdultSources::default(),
        switches: &*flags,
    };
    assert_eq!(
        adult_enrich::check(&mut s, &engine, ctx(QueryForm::Parsed), vague, T).unwrap(),
        Err(Ineligible::NoQuery)
    );
    // A vague name sends nothing in the name form either (ENR-1b).
    assert_eq!(
        adult_enrich::check(&mut s, &engine, ctx(QueryForm::Name), vague, T).unwrap(),
        Err(Ineligible::NoQuery)
    );
    for form in [QueryForm::Parsed, QueryForm::Name] {
        let got = adult_enrich::check(&mut s, &engine, ctx(form), named, T)
            .unwrap()
            .unwrap();
        let text = got.query.text().join(" ");
        assert!(
            !text.contains("Secretfile") && !text.contains("Other"),
            "{form:?}"
        );
    }
}

/// A torrent marked to be classified again waits until the backfill has
/// redone it: its stored type may be stale.
#[test]
fn a_torrent_pending_reclassification_waits() {
    let (mut s, dht, enr) = store();
    let (id, _) = put(&mut s, "Garden Scene", dht, 0, true);
    let engine = stand_in().current();
    let flags = on();
    let ctx = GateCtx {
        manifest: &MANIFEST,
        source: enr,
        form: QueryForm::Name,
        adult: AdultSources::default(),
        switches: &*flags,
    };
    assert!(
        adult_enrich::check(&mut s, &engine, ctx, id, T)
            .unwrap()
            .is_ok()
    );
    s.mark_unenriched(&[id]).unwrap();
    assert_eq!(
        adult_enrich::check(&mut s, &engine, ctx, id, T).unwrap(),
        Err(Ineligible::Pending)
    );
}

/// The mask can't be skipped (ENR-1b): an always-adult source that isn't an
/// enrichment source attaching a section has it dropped, and its claims
/// take the usual path, so the writer still classifies the torrent adult.
#[test]
fn an_always_adult_source_cannot_skip_its_mask_with_a_section() {
    let (mut s, dht, enr) = store();
    let sukebei = s.register_source("sukebei", "1", 0).unwrap();
    let (id, key) = put(&mut s, "Garden Tour 2022", dht, 0, false);
    let engine = stand_in().current();
    let ctx = Ctx {
        adult: AdultSources(1 << sukebei.get()),
        enrichers: EnrichSources(1 << enr.get()),
        ..Ctx::default()
    };
    let claims = lodestar_sources::Claims {
        name: Some("Garden Tour 2022".into()),
        adult: Some(section(Some(Kind2::Scene), "Garden")),
        ..Default::default()
    };
    let d = Discovery {
        source: sukebei,
        found: lodestar_sources::Found::key(key, 0).with_claims(claims),
        at: T,
        reply: false,
    };
    let done = ingest_with(&mut s, &engine, ctx, &d);
    assert!(done.matched.is_empty(), "not the matches path");
    assert_eq!(done.adult, vec![id], "the mask applies");
    let stored = s.torrent_claims(id).unwrap();
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].claims.as_ref().unwrap().adult.is_none(),
        "no section kept"
    );

    // A section alone, from a source that isn't an enrichment source,
    // stores nothing.
    let d = matched(dht, key, section(Some(Kind2::Scene), "Garden"));
    let done = ingest_with(&mut s, &engine, ctx, &d);
    assert!(done.matched.is_empty() && done.touched.is_empty());
    assert!(
        !s.torrent_claims(id)
            .unwrap()
            .iter()
            .any(|c| c.source == dht)
    );
}

/// The generation race (ENR-1b): a classification-rule change that lands
/// after the writer took its engine and before the match is classified
/// can't be overwritten. The match is classified under the live rules read
/// after the generation, so a new Not adult rule keeps the row marked for
/// the backfill instead of writing the stale adult reading over the mark.
#[test]
fn a_rules_change_under_a_match_is_not_overwritten() {
    let (mut s, dht, enr) = store();
    let (id, key) = put(&mut s, "Studiox Garden Scene XXX 1080p", dht, 0, false);
    let filter = stand_in();
    // The writer's snapshot, taken before the batch.
    let stale = filter.current();
    // The rules change in between, as the API makes it: a Not adult rule,
    // a new generation, the rows marked, the live filter updated.
    let rule = s.add_class_rule("garden", false).unwrap();
    s.bump_class_gen().unwrap();
    s.mark_unenriched(&[id]).unwrap();
    filter.update_engine(|e| {
        assert!(
            e.load_class_rules(&[(rule, "garden".into(), false)])
                .is_empty()
        );
    });
    let ctx = Ctx {
        enrichers: EnrichSources(1 << enr.get()),
        filter: Some(&filter),
        ..Ctx::default()
    };
    let done = ingest_with(
        &mut s,
        &stale,
        ctx,
        &matched(enr, key, section(Some(Kind2::Vr), "Garden")),
    );
    assert_eq!(done.matched, vec![id], "the claim is stored");
    let e = s.enrichment(id).unwrap().unwrap();
    assert_ne!(
        e.version,
        lodestar_classify::ENRICHMENT_VERSION,
        "still marked"
    );
    let r = Record::decode(&e.data).unwrap();
    assert_ne!(r.adult_kind, Some(AdultKind::Vr), "no stale pin written");
}

/// Matches left for the backfill raise its flag, and with no live filter
/// they're all left for it.
#[test]
fn deferred_matches_raise_the_backfill_flag() {
    let (mut s, dht, enr) = store();
    // Not stored as adult: becoming adult needs the backfill's judgement.
    let (id, key) = put(&mut s, "Garden Tour 2022", dht, 0, false);
    let engine = stand_in().current();
    let flag = std::sync::atomic::AtomicBool::new(false);
    let ctx = Ctx {
        enrichers: EnrichSources(1 << enr.get()),
        reenrich: Some(&flag),
        ..Ctx::default()
    };
    ingest_with(
        &mut s,
        &engine,
        ctx,
        &matched(enr, key, section(Some(Kind2::Scene), "Garden")),
    );
    assert!(flag.load(Ordering::SeqCst));
    assert_ne!(
        s.enrichment(id).unwrap().unwrap().version,
        lodestar_classify::ENRICHMENT_VERSION
    );

    // No filter: left for the backfill too.
    let (id, key) = put(&mut s, "Garden Scene", dht, 0, true);
    let flag = std::sync::atomic::AtomicBool::new(false);
    let ctx = Ctx {
        enrichers: EnrichSources(1 << enr.get()),
        reenrich: Some(&flag),
        ..Ctx::default()
    };
    let stats = UnverifiedStats::default();
    let d = matched(enr, key, section(Some(Kind2::Scene), "Garden"));
    unverified::ingest(
        &mut s,
        &engine,
        ctx,
        std::slice::from_ref(&d),
        &stats,
        T,
        &|_, _| {},
    )
    .unwrap();
    assert!(flag.load(Ordering::SeqCst));
    assert_ne!(
        s.enrichment(id).unwrap().unwrap().version,
        lodestar_classify::ENRICHMENT_VERSION
    );
}

/// A torrent refused as pending is eligible again once the backfill has
/// classified it again.
#[test]
fn the_backfill_makes_a_pending_torrent_eligible_again() {
    let (mut s, dht, enr) = store();
    let (id, _) = put(&mut s, "Studiox Garden Scene XXX 1080p", dht, 0, false);
    let filter = stand_in();
    let flags = on();
    let ctx = GateCtx {
        manifest: &MANIFEST,
        source: enr,
        form: QueryForm::Name,
        adult: AdultSources::default(),
        switches: &*flags,
    };
    s.mark_unenriched(&[id]).unwrap();
    assert_eq!(
        adult_enrich::check(&mut s, &filter.current(), ctx, id, T).unwrap(),
        Err(Ineligible::Pending)
    );
    let backfill = lodestar_pipeline::enrich::Backfill {
        filter: Some(&filter),
        ..Default::default()
    };
    lodestar_pipeline::enrich::enrich_stored_with(&mut s, 0, 100, &backfill).unwrap();
    assert!(
        adult_enrich::check(&mut s, &filter.current(), ctx, id, T)
            .unwrap()
            .is_ok()
    );
}

/// A commit that rewrote a matched torrent's record without its claims (a
/// partial import's upgrade) is marked so the backfill applies the match
/// again; a torrent without a match isn't touched.
#[test]
fn a_rewritten_match_is_marked_and_applied_again() {
    let (mut s, dht, enr) = store();
    let (with, key) = put(&mut s, "Studiox Garden Scene XXX 1080p", dht, 0, false);
    let (without, _) = put(&mut s, "Studiox Other Scene XXX 1080p", dht, 0, false);
    let filter = stand_in();
    let engine = filter.current();
    ingest(
        &mut s,
        &engine,
        &matched(enr, key, section(Some(Kind2::Vr), "Garden")),
    );
    // The upgrade writes prepare's record, which knows no match.
    let raw = info_dict("Studiox Garden Scene XXX 1080p");
    let info = Info::parse(&raw).unwrap();
    let files = file_infos(&info.files, info.path_encoding);
    let plain = enrich_with_rule("Studiox Garden Scene XXX 1080p", &files, None);
    s.set_enrichments(&[(with, plain)]).unwrap();
    let kind = |s: &Store, id| {
        Record::decode(&s.enrichment(id).unwrap().unwrap().data)
            .unwrap()
            .adult_kind
    };
    assert_ne!(kind(&s, with), Some(AdultKind::Vr));
    let marked = lodestar_pipeline::enrich::mark_matched(&mut s, &[with, without]).unwrap();
    assert_eq!(marked, vec![with]);
    assert_eq!(
        s.enrichment(without).unwrap().unwrap().version,
        lodestar_classify::ENRICHMENT_VERSION
    );
    let backfill = lodestar_pipeline::enrich::Backfill {
        filter: Some(&filter),
        ..Default::default()
    };
    lodestar_pipeline::enrich::enrich_stored_with(&mut s, 0, 100, &backfill).unwrap();
    assert_eq!(kind(&s, with), Some(AdultKind::Vr));
}
