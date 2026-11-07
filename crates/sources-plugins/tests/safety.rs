//! The v0.6 sources safety gate (SRC-G, architecture §18.7, §18.9;
//! `docs/decisions/2026-11-07-SRC-G.md` holds the verdict).
//!
//! Each plugin runs on its recorded fixture through `FakeCtx` (no
//! network), exactly as its own contract tests run it. Its finds are then
//! relayed, unchanged, into a real `Pipeline` with the real filter and an
//! in-memory store, so the properties are checked where they live: intake,
//! the writer's unverified path and its re-judge sweep, the store's hold
//! guard, and the consent gate. The relay keeps the plugin's manifest
//! (id, kinds, `finds_private`, `always_adult`); only its consent class
//! and default switch are opened so it runs. Consent is its own property,
//! checked below on the real manifests.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_filter::{
    Action, Core, CsList, Engine, Handle, Lists, Origin, Sealer, Switches, TermSet,
};
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::{BoxFuture, Consent, Found, Manifest, PluginError, SourceCtx, SourcePlugin};
use lodestar_sources_plugins::{arr, catalogue, full_scrape, nyaa};
use lodestar_store::unverified::Network;
use lodestar_store::{Store, StoreConfig};

#[path = "support/plugins.rs"]
mod plugins;

use plugins::{Run, fixture_runs, relay};

/// This crate's folder, for the shared fixture runs.
const PLUGINS_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The child-safety core with stand-in term lists, as the pipeline's own
/// tests build it: a child role next to a sexual term is a CS-4 pattern.
fn core_with(sexual: &[&str], roles: &[&str]) -> Arc<Handle> {
    let lists = Lists {
        sexual: TermSet::of(sexual),
        child_roles: TermSet::of(roles),
        codes: TermSet::of(&["codex"]),
    };
    Arc::new(Handle::new(Engine::new(Core::new(
        lists,
        CsList::default(),
        Switches::default(),
    ))))
}

fn user_rule(filter: &Handle, query: &str, action: Action) {
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

fn pipeline_config(filter: Arc<Handle>) -> PipelineConfig {
    PipelineConfig {
        flush_every: Duration::from_millis(100),
        batch_wait: Duration::from_millis(20),
        filter,
        sealer: Some(Arc::new(Sealer::new(&Sealer::generate_key().unwrap()))),
        ..PipelineConfig::default()
    }
}

/// Polls for at least 10 s (standards §2), so the slowest runner passes.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..600 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

fn n(c: &AtomicU64) -> u64 {
    c.load(Ordering::Relaxed)
}

/// Lowercase words of four or more letters, in order.
fn words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| w.len() >= 4)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn name_of(f: &Found) -> Option<&str> {
    f.claims.as_deref()?.name.as_deref()
}

/// Whether anything of `key` is stored: a torrent, an unverified item, or
/// claims on either.
fn stored(store: &Store, key: &InfoHash) -> bool {
    store.find(key).unwrap().is_some() || store.unverified(Network::Bt, &key.0).unwrap().is_some()
}

fn held(store: &Store, key: &InfoHash) -> bool {
    store.held_among(&[key.0]).unwrap()[0]
}

/// One plugin kind's injection, decided from its own finds: the first
/// key goes on the floor (the shipped hash blocklist), the first other
/// find with a two-word name is made a core pattern hit, and every other
/// key is the clean control.
struct Plan {
    floor: InfoHash,
    named: Option<(InfoHash, String, String)>,
    clean: Vec<InfoHash>,
}

fn plan(run: &Run) -> Plan {
    let keys: Vec<InfoHash> = run
        .finds
        .iter()
        .map(|f| f.key)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert!(keys.len() >= 2, "{}: too few finds to inject", run.kind);
    let floor = run.finds[0].key;
    let mut named = None;
    for f in &run.finds {
        if f.key == floor {
            continue;
        }
        if let Some(w) = name_of(f).map(words)
            && w.len() >= 2
        {
            named = Some((f.key, w[0].clone(), w[1].clone()));
            break;
        }
    }
    let (role, sexual) = named
        .as_ref()
        .map(|(_, a, b)| (a.clone(), b.clone()))
        .unwrap_or_default();
    // A clean key's name must not carry either stand-in term, or the core
    // would (rightly) hold it too.
    let clean = keys
        .iter()
        .copied()
        .filter(|k| *k != floor && named.as_ref().is_none_or(|(nk, _, _)| nk != k))
        .filter(|k| {
            run.finds.iter().filter(|f| f.key == *k).all(|f| {
                name_of(f).is_none_or(|n| {
                    let w = words(n);
                    !w.contains(&role) && !w.contains(&sexual)
                })
            })
        })
        .collect();
    Plan {
        floor,
        named,
        clean,
    }
}

/// §18.9: a blocklisted hash and a blocklisted name, injected through
/// each plugin kind's recorded fixture, are held before storage and
/// counted; a second sighting is refused; the rest is stored as
/// unverified (never verified, since nothing fetched it); and a rule
/// added after storage removes what it matches (the re-judge sweep).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocklisted_hash_and_name_through_every_plugin_kind_are_never_stored() {
    let runs = fixture_runs();
    let kinds: BTreeSet<_> = runs.iter().map(|r| r.kind).collect();
    assert_eq!(kinds.len(), 9, "{kinds:?}");
    for run in &runs {
        check_blocklist(run).await;
    }
}

async fn check_blocklist(run: &Run) {
    let kind = run.kind;
    let plan = plan(run);
    let (sexual, roles): (Vec<&str>, Vec<&str>) = match &plan.named {
        Some((_, role, sexual)) => (vec![sexual.as_str()], vec![role.as_str()]),
        None => (vec!["lexa"], vec!["rolea"]),
    };
    let filter = core_with(&sexual, &roles);
    filter.update(|c| c.cs.add_hash(plan.floor.0, Origin::Imported));
    let (source, tx) = relay(&run.manifest);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(pipeline_config(filter.clone()), store, vec![source])
        .await
        .unwrap();
    let blocked = 1 + usize::from(plan.named.is_some());
    let gone = || {
        let i = pipeline.intake_stats();
        let u = &pipeline.writer_stats().unverified;
        n(&i.refused) + n(&u.held) + n(&u.refused) + n(&u.dropped)
    };
    tx.send(run.finds.clone()).await.unwrap();
    until(&format!("{kind}: the finds judged"), || {
        let u = &pipeline.writer_stats().unverified;
        gone() >= blocked as u64 && n(&u.stored) + n(&u.existing) >= plan.clean.len() as u64
    })
    .await;
    // A second sighting of every find: the holds refuse the blocked keys.
    let before = gone();
    tx.send(run.finds.clone()).await.unwrap();
    until(&format!("{kind}: the second sighting refused"), || {
        gone() >= before + blocked as u64
    })
    .await;
    // Every find has reached the writer, which judges a batch under the
    // rules as they were when it started: so only the re-judge sweep can
    // apply a rule added now.
    let sent = 2 * run.finds.len() as u64;
    until(&format!("{kind}: every find with the writer"), || {
        let i = pipeline.intake_stats();
        let u = &pipeline.writer_stats().unverified;
        n(&i.refused) + n(&i.claims) >= sent && n(&u.received) >= n(&i.claims)
    })
    .await;
    // A rule added after storage: the sweep removes the first clean key.
    let target = plan.clean.first().copied();
    let rule_word = target.and_then(|k| {
        run.finds
            .iter()
            .filter(|f| f.key == k)
            .find_map(|f| name_of(f).map(words))
            .and_then(|w| w.into_iter().next())
    });
    if let Some(w) = &rule_word {
        let refiled = n(&pipeline.writer_stats().unverified.refiled);
        user_rule(&filter, w, Action::Drop);
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let k = target.unwrap();
        // Gone, and by the sweep: its count follows the commit.
        until(&format!("{kind}: the re-judge sweep removes {k:?}"), || {
            !stored(&reader, &k) && n(&pipeline.writer_stats().unverified.refiled) > refiled
        })
        .await;
    }
    let store = pipeline.shutdown().await.unwrap();

    // The floor is itself the permanent hold: intake refuses its hash
    // before anything is written, so there's no tombstone to keep.
    assert!(!stored(&store, &plan.floor), "{kind}: the floor hash");
    if let Some((k, _, _)) = &plan.named {
        assert!(held(&store, k), "{kind}: the blocked name is held");
        assert!(!stored(&store, k), "{kind}: the blocked name");
        // Held under the core's rule, with the name only sealed.
        let q = store.quarantined(64).unwrap();
        let q = q.iter().find(|q| q.hash == k.0).expect("quarantined");
        assert!(q.rule.starts_with("CS-"), "{kind}: {}", q.rule);
    }
    for k in &plan.clean {
        let removed = Some(*k) == target && rule_word.is_some();
        if removed {
            // Removed by the user's rule after storage. The word may name
            // more than one clean key; each is gone.
            assert!(!stored(&store, k), "{kind}: the re-judged key stays");
            continue;
        }
        let item = store.unverified(Network::Bt, &k.0).unwrap();
        let w = rule_word.as_deref().unwrap_or("\u{0}");
        let matched = run
            .finds
            .iter()
            .filter(|f| f.key == *k)
            .any(|f| name_of(f).is_some_and(|n| words(n).iter().any(|x| x == w)));
        if matched {
            assert!(item.is_none(), "{kind}: the rule's other match stays");
        } else {
            assert!(item.is_some(), "{kind}: a clean key isn't stored");
            assert!(!held(&store, k), "{kind}: a clean key is held");
        }
        // Never verified: no fetch happened.
        assert!(store.find(k).unwrap().is_none(), "{kind}: promoted");
    }
}

// ---------------------------------------------------------------------
// `private_source` isolation (§18.7).
// ---------------------------------------------------------------------

/// A peer finder (the DHT's role) that records every key it's asked about
/// and finds no peers.
struct Spy {
    manifest: Manifest,
    asked: Arc<Mutex<Vec<InfoHash>>>,
}

fn spy() -> (Arc<Spy>, Arc<Mutex<Vec<InfoHash>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let manifest = Manifest {
        id: "spy",
        name: "spy",
        kinds: &[lodestar_sources::Kind::PeerFinder],
        consent: Consent::None,
        enabled_by_default: true,
        finds_private: false,
        always_adult: false,
        ..arr::ArrWebhooks::new().manifest().clone()
    };
    let s = Spy {
        manifest,
        asked: asked.clone(),
    };
    (Arc::new(s), asked)
}

impl SourcePlugin for Spy {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        let mut requests = ctx.peer_requests.take().expect("a peer finder");
        let asked = self.asked.clone();
        Box::pin(async move {
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(r) = requests.recv() => {
                        asked.lock().unwrap().push(r.key);
                        let _ = r.admit.send(None);
                    }
                }
            }
        })
    }
}

/// The plugin kinds whose every find is private (§18.7): download
/// clients, the webhooks, and a manager's private indexers.
fn private_runs(runs: &[Run]) -> Vec<&Run> {
    runs.iter()
        .filter(|r| r.finds.iter().any(|f| f.private_source))
        .collect()
}

/// A key found only by a private source is stored private, keeps no
/// tracker or outside link, and is never handed to a public peer finder
/// (DHT `get_peers`, announce), whether it came with claims or bare. A
/// public key-only find, sent after them, is: so the finder was asked
/// and the private keys weren't among what it was asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_private_find_is_stored_private_and_never_reaches_a_public_lookup() {
    let runs = fixture_runs();
    let private = private_runs(&runs);
    let kinds: BTreeSet<_> = private.iter().map(|r| r.kind).collect();
    // Prowlarr's fixture has one private indexer; the clients and the
    // webhooks are private through and through.
    assert_eq!(
        kinds,
        BTreeSet::from([
            "arr",
            "clients/qbittorrent",
            "clients/transmission",
            "prowlarr"
        ])
    );
    for r in &private {
        if r.kind != "prowlarr" {
            assert!(r.manifest.finds_private, "{}", r.kind);
            assert!(r.finds.iter().all(|f| f.private_source), "{}", r.kind);
        }
    }
    let (finder, asked) = spy();
    let mut plugins: Vec<Arc<dyn SourcePlugin>> = vec![finder];
    let mut senders = Vec::new();
    for r in &private {
        let (p, tx) = relay(&r.manifest);
        plugins.push(p);
        senders.push((r, tx));
    }
    let (public, public_tx) = relay(&nyaa::Nyaa::new().manifest().clone());
    plugins.push(public);
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(
        pipeline_config(core_with(&["lexa"], &["rolea"])),
        store,
        plugins,
    )
    .await
    .unwrap();
    let mut private_keys = BTreeSet::new();
    let mut public_keys = BTreeSet::new();
    for (r, tx) in &senders {
        let mut round = r.finds.clone();
        // Each find again as a bare sighting, which would go to the pool.
        round.extend(r.finds.iter().map(|f| Found {
            claims: None,
            ..f.clone()
        }));
        for f in &round {
            match f.private_source {
                true => private_keys.insert(f.key),
                false => public_keys.insert(f.key),
            };
        }
        tx.send(round).await.unwrap();
    }
    let control = InfoHash([0x77; 20]);
    until("the private claims stored", || {
        n(&pipeline.writer_stats().unverified.stored) >= private_keys.len() as u64
    })
    .await;
    public_tx.send(vec![Found::key(control, 0)]).await.unwrap();
    until("the public key looked up", || {
        asked.lock().unwrap().contains(&control)
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    let asked = asked.lock().unwrap().clone();
    for k in &private_keys {
        assert!(!asked.contains(k), "{k:?} reached a public peer finder");
        let item = store
            .unverified(Network::Bt, &k.0)
            .unwrap()
            .expect("stored");
        assert!(item.private_source && item.claims_private, "{k:?}");
        if let Some(c) = &item.claims {
            assert!(c.trackers.is_empty(), "{k:?} kept a tracker");
        }
    }
    // Prowlarr's public indexer's finds stay public: the flag is per find.
    for k in public_keys.difference(&private_keys) {
        let item = store
            .unverified(Network::Bt, &k.0)
            .unwrap()
            .expect("stored");
        assert!(!item.private_source, "{k:?}");
    }
    // Unverified items aren't torrents: search, counts, Insights, feeds
    // and exports read only `torrents` (SRC-3b will index them, hidden
    // unless asked for).
    let torrents: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM torrents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(torrents, 0);
}

// ---------------------------------------------------------------------
// No profiles of people (§18.7, §18.9's no-profiles test).
// ---------------------------------------------------------------------

/// Column-name parts that would name a person's or a peer's address.
const ADDRESS_WORDS: &[&str] = &[
    "ip", "ipv4", "ipv6", "addr", "address", "peer", "peers", "port", "node", "endpoint", "remote",
    "client", "sharer", "user", "username", "mac",
];

/// Columns that match a word above but hold no address: what each is.
const NOT_ADDRESSES: &[(&str, &str, &str)] = &[
    ("swarm", "dht_peers", "a count of DHT peers, aggregate"),
    ("swarm", "dht_seeds", "a count of DHT seeds, aggregate"),
];

/// Tables whose name matches a word above but hold no address.
const NOT_ADDRESS_TABLES: &[(&str, &str)] = &[(
    "user_items",
    "the operator's own marks on torrents (favourite, hidden, labels, note)",
)];

/// No table or column of the latest schema stores a peer's or a person's
/// address. Swarm figures are aggregate counts (§18.7).
#[test]
fn the_schema_keeps_no_peer_address_beside_content() {
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let db = store.connection();
    let mut tables = db
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")
        .unwrap();
    let tables: Vec<String> = tables
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(tables.len() >= 20, "{tables:?}");
    let mut found = Vec::new();
    for t in &tables {
        let mut cols = db
            .prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))
            .unwrap();
        let cols: Vec<String> = cols
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for c in cols {
            let lower = c.to_ascii_lowercase();
            if lower.split('_').any(|w| ADDRESS_WORDS.contains(&w))
                && !NOT_ADDRESSES.iter().any(|(nt, nc, _)| nt == t && *nc == c)
            {
                found.push(format!("{t}.{c}"));
            }
        }
        let lower = t.to_ascii_lowercase();
        if lower.split('_').any(|w| ADDRESS_WORDS.contains(&w))
            && !NOT_ADDRESS_TABLES.iter().any(|(nt, _)| nt == t)
        {
            found.push(t.clone());
        }
    }
    assert!(found.is_empty(), "address-like columns: {found:?}");
}

/// Every event the pipeline, filter, DHT, peer wire and integrations
/// declare is on, and every record is kept.
struct Capture(Mutex<Vec<(String, String, bool)>>);

impl lodestar_log::Sink for Capture {
    fn record(&self, r: &lodestar_log::Record<'_>) {
        let all = lodestar_log::Mask(u32::MAX).without_opt_ins();
        self.0.lock().unwrap().push((
            r.text(all, None),
            r.text(lodestar_log::Mask::PRIVATE, None),
            r.pairs_torrent(),
        ));
    }
}

/// The log scan (§18.9): a fixture run with every event on, a failed
/// fetch from a peer included, shows no peer address beside a torrent
/// unless the opt-in Peer activity kind is on, even with every other kind
/// (hashes and peer addresses included) shown. With the default preset
/// no key a source found is printed at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_log_scan_finds_no_peer_address_beside_a_torrent() {
    static CAPTURE: std::sync::OnceLock<Capture> = std::sync::OnceLock::new();
    let capture = CAPTURE.get_or_init(|| Capture(Mutex::new(Vec::new())));
    lodestar_log::set_sink(capture).expect("this binary's only sink");
    for c in [
        &lodestar_pipeline::log_events::CATALOGUE,
        &lodestar_filter::log_events::CATALOGUE,
        &lodestar_dht::log_events::CATALOGUE,
        &lodestar_peer_wire::log_events::CATALOGUE,
        &lodestar_integrations::log_events::CATALOGUE,
    ] {
        for e in c.events {
            e.set_on(true);
        }
    }
    // A peer that refuses: its address and the key meet in a failed fetch.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = closed.local_addr().unwrap();
    drop(closed);
    let runs = fixture_runs();
    let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
    let mut senders = Vec::new();
    for r in &runs {
        let (p, tx) = relay(&r.manifest);
        plugins.push(p);
        senders.push((r, tx));
    }
    let mut config = pipeline_config(core_with(&["lexa"], &["rolea"]));
    config.fetch.allow_private = true;
    config.pool.max_attempts = 1;
    let fetched = InfoHash([0x66; 20]);
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config, store, plugins).await.unwrap();
    let mut keys: BTreeSet<InfoHash> = BTreeSet::from([fetched]);
    for (r, tx) in &senders {
        keys.extend(r.finds.iter().map(|f| f.key));
        let mut round = r.finds.clone();
        if r.kind == "nyaa" {
            round.push(Found::key(fetched, 0).with_peers(vec![peer]));
        }
        tx.send(round).await.unwrap();
    }
    until("the failed fetch logged", || {
        capture
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(all, _, pairs)| *pairs && all.contains("Metadata fetch failed"))
    })
    .await;
    pipeline.shutdown().await.unwrap();
    let lines = capture.0.lock().unwrap().clone();
    let address = peer.ip().to_string();
    for (all, private, pairs) in &lines {
        if *pairs {
            assert!(!all.contains(&address), "a peer beside a torrent: {all}");
        }
        // The address shows only where no torrent is named at all.
        let names_a_key = keys.iter().any(|k| all.contains(&k.to_string()));
        assert!(
            !(names_a_key && all.contains(&address)),
            "a peer beside a key: {all}"
        );
        for k in &keys {
            assert!(
                !private.contains(&k.to_string()),
                "a key at the default preset: {private}"
            );
        }
    }
    // The scan saw the case it's for: a peer and a key in one record,
    // with the key shown and the peer not.
    assert!(
        lines.iter().any(|(all, _, pairs)| *pairs
            && all.contains(&fetched.to_string())
            && all.contains("peer=[hidden]")),
        "{lines:?}"
    );
}

// ---------------------------------------------------------------------
// Consent can't be bypassed (§18.7, §18.9; owner §2.49 and
// X-consent-gate-052's v0.5.2 rule).
// ---------------------------------------------------------------------

/// A plugin with a real manifest that only notes that it ran.
struct Probe {
    manifest: Manifest,
    ran: Arc<AtomicBool>,
}

impl SourcePlugin for Probe {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        self.ran.store(true, Ordering::SeqCst);
        Box::pin(async move {
            ctx.shutdown.requested().await;
            Ok(())
        })
    }
}

/// Every plugin in the catalogue, with its real consent class: a state
/// an earlier build or a restored backup left Enabled is switched off at
/// the start before it runs, and no `PATCH` (any consent, any version,
/// with or without the password) turns it on, unless its class needs no
/// consent. Settings import and the config file have no path to a
/// source's state (`docs/decisions/2026-11-07-SRC-G.md`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_enable_path_opens_a_consent_class_in_any_shipped_plugin() {
    use lodestar_pipeline::control::{CONSENT_GATE_REASON, SourceChange, SourceError};
    use lodestar_pipeline::{Control, Pipeline as P};
    use lodestar_sources::consent;
    use lodestar_store::SourceState;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let manifests: Vec<&'static Manifest> = catalogue::manifests().collect();
    assert!(manifests.len() >= 11);
    let ids: BTreeSet<_> = manifests.iter().map(|m| m.id).collect();
    assert_eq!(ids.len(), manifests.len(), "ids repeat");
    // Every one left Enabled, as an earlier build or a backup could.
    {
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        for m in &manifests {
            store.register_source(m.id, m.version, 0).unwrap();
        }
    }
    let mut ran = Vec::new();
    let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
    for m in &manifests {
        let flag = Arc::new(AtomicBool::new(false));
        ran.push((*m, flag.clone()));
        plugins.push(Arc::new(Probe {
            manifest: (*m).clone(),
            ran: flag,
        }));
    }
    let pipeline = P::start(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        plugins,
    )
    .await
    .unwrap();
    let open: Vec<_> = ran
        .iter()
        .filter(|(m, _)| m.consent == Consent::None)
        .collect();
    assert!(!open.is_empty());
    until("the no-consent sources run", || {
        open.iter().all(|(_, r)| r.load(Ordering::SeqCst))
    })
    .await;
    let control = Control::start(
        pipeline.monitor(),
        None,
        Arc::new(Mutex::new(
            Store::open(&db, StoreConfig::default()).unwrap(),
        )),
        false,
        lodestar_dht::node::Intensity::Balanced,
    )
    .await
    .unwrap();
    let classes = [
        None,
        Some(Consent::None),
        Some(Consent::External),
        Some(Consent::Account),
        Some(Consent::Restricted),
        Some(Consent::Overlay),
    ];
    for view in control.sources().unwrap() {
        let m = view.status.manifest.clone();
        let (_, flag) = ran.iter().find(|(r, _)| r.id == m.id).unwrap();
        if m.consent == Consent::None {
            assert_eq!(view.state, SourceState::Enabled, "{}", m.id);
            continue;
        }
        assert_ne!(
            view.state,
            SourceState::Enabled,
            "{}: on after the start",
            m.id
        );
        assert!(!flag.load(Ordering::SeqCst), "{} ran", m.id);
        for class in classes {
            for version in [None, Some(String::new()), Some(consent::version(&m))] {
                for password_confirmed in [false, true] {
                    let r = control
                        .update_source(
                            view.status.id,
                            SourceChange {
                                enabled: Some(true),
                                consent: class,
                                consent_version: version.clone(),
                                password_confirmed,
                                ..SourceChange::default()
                            },
                        )
                        .await;
                    assert!(
                        matches!(&r, Err(SourceError::Invalid { field: "enabled", reason })
                            if *reason == CONSENT_GATE_REASON),
                        "{}: {r:?}",
                        m.id
                    );
                }
            }
        }
        // A new consent text, or a changed address list, asks again: an
        // acknowledgement of the old one doesn't cover it.
        if consent::needs_ack(m.consent) {
            let now = consent::Ack::of(&m);
            assert!(now.covers(&m));
            let older = consent::Ack {
                version: now.version.replacen(
                    &format!("t{}", consent::text_version(m.consent)),
                    &format!("t{}", consent::text_version(m.consent) - 1),
                    1,
                ),
                ..now.clone()
            };
            assert!(!older.covers(&m), "{}", m.id);
            assert!(consent::need(&m, Some(&older), &[]).is_some(), "{}", m.id);
            let moved = Manifest {
                endpoints: &[],
                ..m.clone()
            };
            assert!(!now.covers(&moved) || m.endpoints.is_empty(), "{}", m.id);
        }
    }
    drop(control);
    pipeline.shutdown().await.unwrap();
    // Nothing the refusals touched was written.
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    for row in store.sources().unwrap() {
        let m = manifests.iter().find(|m| m.id == row.plugin).unwrap();
        assert_eq!(
            row.state == SourceState::Enabled,
            m.consent == Consent::None,
            "{}",
            m.id
        );
    }
}

// ---------------------------------------------------------------------
// The full scrape stays unwired until its private-tracker lookup is
// the store's (runner security pre-audit, 2026-11-05; owner §2.37).
// ---------------------------------------------------------------------

fn rust_files(dir: &std::path::Path, out: &mut Vec<(PathBuf, String)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "tests") {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let name = p.file_name().unwrap().to_string_lossy();
            if name == "tests.rs" || name.ends_with("_tests.rs") {
                continue;
            }
            // What the binary is built from: a test module (by convention
            // last in its file) is left out.
            let text = std::fs::read_to_string(&p).unwrap();
            let shipped = text.split("#[cfg(test)]").next().unwrap().to_owned();
            out.push((p.clone(), shipped));
        }
    }
}

/// `full_scrape` isn't registered by the binary today. The moment it is,
/// this fails unless the binary also has a store-backed `PrivateTrackers`
/// (never the test-only `StaticPrivateTrackers`, which the `testing`
/// feature keeps out of it), and the settings text and the user guide tell
/// the user not to add a tracker whose published rules forbid scraping.
#[test]
fn the_full_scrape_is_unwired_or_wired_with_a_store_backed_private_tracker_lookup() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let host = root.join("crates/lodestar");
    let cargo = std::fs::read_to_string(host.join("Cargo.toml")).unwrap();
    let dep = cargo
        .lines()
        .find(|l| l.starts_with("lodestar-sources-plugins"))
        .expect("the host uses the plugins");
    assert!(!dep.contains("testing"), "{dep}");
    let mut files = Vec::new();
    rust_files(&host.join("src"), &mut files);
    assert!(files.len() > 20);
    let wired: Vec<_> = files
        .iter()
        .filter(|(_, t)| t.contains("FullScrape::") || t.contains("full_scrape::FullScrape"))
        .map(|(p, _)| p.display().to_string())
        .collect();
    assert!(
        files
            .iter()
            .all(|(_, t)| !t.contains("StaticPrivateTrackers")),
        "the test-only lookup in the binary"
    );
    if wired.is_empty() {
        return;
    }
    let store_backed = files.iter().any(|(_, t)| {
        t.contains("impl PrivateTrackers for") || t.contains("full_scrape::PrivateTrackers for")
    });
    assert!(
        store_backed,
        "{wired:?}: full_scrape wired without a store-backed PrivateTrackers"
    );
    let rules = "published rules";
    assert!(
        full_scrape::CONFIG_SCHEMA.contains(rules),
        "the full-scrape settings text lacks the §2.37 wording"
    );
    let mut guide = String::new();
    for e in std::fs::read_dir(root.join("docs/user")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "md") {
            guide.push_str(&std::fs::read_to_string(p).unwrap());
        }
    }
    assert!(
        guide.contains("full scrape") && guide.contains(rules),
        "the user guide lacks the §2.37 wording"
    );
}

/// Torznab and Stremio don't exist yet (§18.7 names them among the
/// outputs `private_source` never reaches). The day either is routed in
/// the server, this fails until its slice adds the leak test the gate asks
/// for and lists it here.
#[test]
fn torznab_and_stremio_outputs_are_not_built_yet() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Each output's private_source test, once it exists: none yet.
    const COVERED: &[&str] = &[];
    let mut files = Vec::new();
    for c in ["crates/api/src", "crates/lodestar/src"] {
        rust_files(&root.join(c), &mut files);
    }
    for output in ["torznab", "stremio"] {
        if COVERED.contains(&output) {
            continue;
        }
        let routed: Vec<_> = files
            .iter()
            .filter(|(_, t)| {
                t.contains(&format!("\"/{output}")) || t.contains(&format!("mod {output};"))
            })
            .map(|(p, _)| p.display().to_string())
            .collect();
        assert!(
            routed.is_empty(),
            "{output} is routed in {routed:?}: add its private_source leak test (SRC-G)"
        );
    }
}
