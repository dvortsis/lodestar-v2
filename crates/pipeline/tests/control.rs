//! Stored source states are enforced (a source that isn't enabled isn't
//! run), and the consent rule for enabling one (§18.1, §18.7, §18.9): no
//! class's condition can be skipped by enabling, by a crash during first
//! registration, or by a restart.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_dht::node::Intensity;
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::control::{SourceChange, SourceError};
use lodestar_pipeline::{Control, Pipeline, PipelineConfig};
use lodestar_sources::consent::{self, Ack, Secret, SecretStore, SourceCredentials};
use lodestar_sources::{
    BoxFuture, Consent, Fetcher, Kind, Manifest, Network, PluginError, Priority, Risk, SourceCtx,
    SourcePlugin,
};
use lodestar_store::{SourceState, Store, StoreConfig};

/// Records whether it ran.
struct Probe {
    manifest: Manifest,
    ran: Arc<AtomicBool>,
}

fn probe(id: &'static str, consent: Consent) -> (Arc<Probe>, Arc<AtomicBool>) {
    probe_with(id, consent, false)
}

fn probe_with(
    id: &'static str,
    consent: Consent,
    enabled_by_default: bool,
) -> (Arc<Probe>, Arc<AtomicBool>) {
    let ran = Arc::new(AtomicBool::new(false));
    let p = Probe {
        manifest: Manifest {
            id,
            name: id,
            version: "1",
            kinds: &[Kind::Discovery],
            networks: &[Network::Bt],
            endpoints: &[],
            risk: Risk::High,
            consent,
            default_priority: Priority::Low,
            enabled_by_default,
        },
        ran: ran.clone(),
    };
    (Arc::new(p), ran)
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

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(
        &self,
        _peer: SocketAddr,
        _key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        Box::pin(async { Err(FailureKind::Refused) })
    }
}

#[tokio::test]
async fn sources_that_arent_enabled_dont_run_and_consent_gates_enabling() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let off = store.register_source("off", "1", 0).unwrap();
    store.set_source_state(off, SourceState::Disabled).unwrap();
    let held = store.register_source("held", "1", 0).unwrap();
    store.set_source_state(held, SourceState::Paused).unwrap();
    drop(store);

    let (on, on_ran) = probe_with("on", Consent::None, true);
    // New, and off by default: registered disabled, so never run.
    let (fresh, fresh_ran) = probe("fresh", Consent::Overlay);
    let (disabled, off_ran) = probe("off", Consent::Restricted);
    let (paused, held_ran) = probe("held", Consent::External);
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![on, fresh, disabled, paused],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(on_ran.load(Ordering::SeqCst));
    assert!(!fresh_ran.load(Ordering::SeqCst), "off by default");
    assert!(
        !off_ran.load(Ordering::SeqCst),
        "a disabled source is skipped"
    );
    assert!(
        !held_ran.load(Ordering::SeqCst),
        "a paused source is skipped"
    );
    let statuses = pipeline.sources();
    let by = |id: &str| statuses.iter().find(|s| s.plugin == id).unwrap();
    assert!(by("on").enabled && by("on").running);
    assert!(!by("off").enabled && !by("off").running);
    assert!(!by("held").enabled && !by("held").running);
    assert!(!by("fresh").enabled);

    let control_store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let control = Control::start(
        pipeline.monitor(),
        None,
        control_store,
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    // A Restricted source needs consent to its own class, at the current
    // version, accepted with the password.
    let version = consent::version(&probe("off", Consent::Restricted).0.manifest);
    let enable = |consent, version: Option<&str>, password_confirmed| SourceChange {
        enabled: Some(true),
        consent,
        consent_version: version.map(String::from),
        password_confirmed,
        ..SourceChange::default()
    };
    for (consent, v) in [
        (None, None),
        (Some(Consent::Restricted), None),
        (Some(Consent::Restricted), Some("0-0000000000000000")),
        (Some(Consent::External), Some(version.as_str())),
        (Some(Consent::Overlay), Some(version.as_str())),
    ] {
        assert!(
            matches!(
                control.update_source(off, enable(consent, v, true)).await,
                Err(SourceError::ConsentRequired { class: Consent::Restricted, version: ref want })
                    if *want == version
            ),
            "{consent:?} {v:?}"
        );
    }
    assert!(matches!(
        control
            .update_source(
                off,
                enable(Some(Consent::Restricted), Some(&version), false)
            )
            .await,
        Err(SourceError::PasswordRequired)
    ));
    let view = control
        .update_source(off, enable(Some(Consent::Restricted), Some(&version), true))
        .await
        .unwrap();
    assert_eq!(view.state, SourceState::Enabled);
    assert_eq!(view.need, None);
    assert!(!view.status.running, "it starts at the next start");
    // Switched off and on again: the acknowledgement stands, but the
    // password is asked every time.
    let off_change = SourceChange {
        enabled: Some(false),
        ..SourceChange::default()
    };
    control
        .update_source(off, off_change.clone())
        .await
        .unwrap();
    assert!(matches!(
        control.update_source(off, enable(None, None, false)).await,
        Err(SourceError::PasswordRequired)
    ));
    let view = control
        .update_source(off, enable(None, None, true))
        .await
        .unwrap();
    assert_eq!(view.state, SourceState::Enabled);
    // An External source needs its endpoint list acknowledged, with no
    // password.
    assert!(matches!(
        control.update_source(held, enable(None, None, false)).await,
        Err(SourceError::ConsentRequired {
            class: Consent::External,
            ..
        })
    ));
    let external = consent::version(&probe("held", Consent::External).0.manifest);
    let view = control
        .update_source(
            held,
            enable(Some(Consent::External), Some(&external), false),
        )
        .await
        .unwrap();
    assert_eq!(view.state, SourceState::Enabled);
    // Disabling needs no consent; an unknown id is refused.
    let view = control
        .update_source(
            held,
            SourceChange {
                enabled: Some(false),
                priority: Some(Priority::Max),
                ..SourceChange::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (view.state, view.priority),
        (SourceState::Disabled, Priority::Max)
    );
    assert!(matches!(
        control
            .update_source(
                lodestar_store::SourceId::new(40).unwrap(),
                SourceChange::default()
            )
            .await,
        Err(SourceError::Unknown)
    ));
    let store = pipeline.shutdown().await.unwrap();
    drop(store);

    // At the next start the stored states and the saved priority hold.
    let (on, _) = probe_with("on", Consent::None, true);
    let (disabled, off_ran) = probe("off", Consent::Restricted);
    let (paused, held_ran) = probe("held", Consent::External);
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![on, disabled, paused],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    until("the consented source runs", || {
        off_ran.load(Ordering::SeqCst)
    })
    .await;
    assert!(!held_ran.load(Ordering::SeqCst));
    let statuses = pipeline.sources();
    let held = statuses.iter().find(|s| s.plugin == "held").unwrap();
    assert_eq!(held.priority, Priority::Max, "the saved priority");
    pipeline.shutdown().await.unwrap();
}

/// The stored state of `plugin`.
fn stored(db: &std::path::Path, plugin: &str) -> SourceState {
    Store::open(db, StoreConfig::default())
        .unwrap()
        .sources()
        .unwrap()
        .into_iter()
        .find(|r| r.plugin == plugin)
        .unwrap()
        .state
}

#[tokio::test]
async fn first_registration_is_one_write_so_a_crash_cant_leave_a_gated_source_on() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    // The "crash": no write after the registration's own can change a
    // source's state. Before SRC-4 a new plugin was inserted enabled and
    // disabled by a second write, which this refuses.
    store
        .connection()
        .execute_batch(
            "CREATE TEMP TRIGGER crash BEFORE UPDATE OF state ON main.sources \
             BEGIN SELECT RAISE(ABORT, 'crashed'); END;",
        )
        .unwrap();
    let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
    let mut ran = Vec::new();
    for (id, class) in [
        ("ext", Consent::External),
        ("acct", Consent::Account),
        ("restr", Consent::Restricted),
        ("over", Consent::Overlay),
    ] {
        // Even one that says it's on by default.
        let (p, r) = probe_with(id, class, true);
        plugins.push(p);
        ran.push(r);
    }
    let (on, on_ran) = probe_with("on", Consent::None, true);
    plugins.push(on);
    let pipeline =
        Pipeline::start_with(PipelineConfig::default(), store, plugins, Arc::new(NoFetch))
            .await
            .unwrap();
    until("the None source runs", || on_ran.load(Ordering::SeqCst)).await;
    for s in pipeline.sources() {
        assert_eq!(s.enabled, s.plugin == "on", "{}", s.plugin);
    }
    assert!(ran.iter().all(|r| !r.load(Ordering::SeqCst)));
    drop(pipeline.shutdown().await.unwrap());
    // What's on disk after the crash: registered, and off.
    for id in ["ext", "acct", "restr", "over"] {
        assert_eq!(stored(&db, id), SourceState::Disabled, "{id}");
    }
    assert_eq!(stored(&db, "on"), SourceState::Enabled);
}

#[tokio::test]
async fn a_restart_switches_off_a_source_whose_condition_no_longer_holds() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let classes = [
        ("ext", Consent::External),
        ("acct", Consent::Account),
        ("restr", Consent::Restricted),
        ("over", Consent::Overlay),
        ("current", Consent::Restricted),
    ];
    {
        // Enabled some other way (an older build, a hand edit, an import
        // that registered it): no record, or a stale one.
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        for (id, _) in classes {
            store.register_source(id, "1", 0).unwrap();
        }
        let stale = |class| {
            Ack {
                version: "0-0000000000000000".into(),
                ..Ack::of(&probe("x", class).0.manifest)
            }
            .encode()
        };
        let restr = store.sources().unwrap()[2].id;
        store
            .enable_source(restr, Some(&stale(Consent::Restricted)))
            .unwrap();
        // One with a current acknowledgement stays on.
        let current = store.sources().unwrap()[4].id;
        let ack = Ack::of(&probe("current", Consent::Restricted).0.manifest).encode();
        store.enable_source(current, Some(&ack)).unwrap();
        // A declined one counts as none.
        let over = store.sources().unwrap()[3].id;
        let declined = Ack {
            accepted: false,
            ..Ack::of(&probe("over", Consent::Overlay).0.manifest)
        };
        store.enable_source(over, Some(&declined.encode())).unwrap();
        // SOCKS5 credentials alone don't make an Account source ready.
        let acct = store.sources().unwrap()[1].id;
        store
            .update_source_secrets(acct, &[(consent::SOCKS5_USERNAME, b"sealed")], &[], None)
            .unwrap();
    }
    let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
    let mut ran = Vec::new();
    for (id, class) in classes {
        let (p, r) = probe(id, class);
        plugins.push(p);
        ran.push((id, r));
    }
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        plugins,
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    let (_, current_ran) = &ran[4];
    until("the acknowledged source runs", || {
        current_ran.load(Ordering::SeqCst)
    })
    .await;
    for s in pipeline.sources() {
        assert_eq!(s.enabled, s.plugin == "current", "{}", s.plugin);
    }
    for (id, r) in &ran[..4] {
        assert!(!r.load(Ordering::SeqCst), "{id} ran");
    }
    drop(pipeline.shutdown().await.unwrap());
    for (id, _) in &classes[..4] {
        assert_eq!(stored(&db, id), SourceState::Disabled, "{id}");
    }
}

#[tokio::test]
async fn an_account_source_needs_credentials_and_losing_them_disables_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (acct, _) = probe("acct", Consent::Account);
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![acct],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    let id = pipeline.sources()[0].id;
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let control = Control::start(
        pipeline.monitor(),
        None,
        store.clone(),
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    let creds =
        SourceCredentials::with_keys(store.clone(), Arc::new(SecretStore::from_key(&[3; 32])));
    let on = SourceChange {
        enabled: Some(true),
        // An acknowledgement of any class doesn't stand in for them.
        consent: Some(Consent::Account),
        ..SourceChange::default()
    };
    assert!(matches!(
        control.update_source(id, on.clone()).await,
        Err(SourceError::CredentialsRequired)
    ));
    // A bad name is refused, naming no value.
    let bad = control
        .update_credentials(
            id,
            &creds,
            vec![("Bad Name".into(), Some(Secret::new("v")))],
        )
        .await;
    assert!(matches!(bad, Err(SourceError::Credential(_))));
    let view = control
        .update_credentials(
            id,
            &creds,
            vec![("api_key".into(), Some(Secret::new("the-key-value")))],
        )
        .await
        .unwrap();
    assert_eq!(view.credentials, ["api_key"]);
    assert_eq!(view.need, None);
    assert!(!format!("{view:?}").contains("the-key-value"));
    assert_eq!(
        creds.get("acct", "api_key").unwrap().unwrap().expose(),
        "the-key-value"
    );
    let view = control.update_source(id, on).await.unwrap();
    assert_eq!(view.state, SourceState::Enabled);
    // Removing it disables the source in the same write.
    let view = control
        .update_credentials(id, &creds, vec![("api_key".into(), None)])
        .await
        .unwrap();
    assert_eq!(view.state, SourceState::Disabled);
    assert!(view.credentials.is_empty());
    // At most 16.
    let many = (0..17)
        .map(|i| (format!("k{i}"), Some(Secret::new("v"))))
        .collect();
    assert!(matches!(
        control.update_credentials(id, &creds, many).await,
        Err(SourceError::Credential(consent::CredentialRefusal::TooMany))
    ));
    pipeline.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_event_write_still_applies_the_change() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (on, _) = probe_with("on", Consent::None, true);
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![on],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    let control_store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let control = Control::start(
        pipeline.monitor(),
        None,
        control_store.clone(),
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    // Every events write now fails.
    control_store
        .lock()
        .unwrap()
        .connection()
        .execute_batch(
            "CREATE TEMP TRIGGER no_events BEFORE INSERT ON main.events \
             BEGIN SELECT RAISE(ABORT, 'events refused'); END;",
        )
        .unwrap();

    assert!(control.set_paused(true).await, "applied despite the error");
    assert!(control.status().paused);
    assert_eq!(control.rates(), (None, None), "paused: a gap");
    assert!(control.note_intensity(Intensity::Aggressive).await);
    // Nothing was recorded, and a repeat is still a no-op.
    assert!(!control.set_paused(true).await);
    assert!(!control.note_intensity(Intensity::Aggressive).await);
    use lodestar_pipeline::event::{INTENSITY, PAUSE, RESUME};
    let recorded = |kinds: &[u32]| control_store.lock().unwrap().last_event(kinds).unwrap();
    assert_eq!(recorded(&[PAUSE, RESUME, INTENSITY]), None);
    // Once writes work again, the next change is recorded as usual.
    control_store
        .lock()
        .unwrap()
        .connection()
        .execute_batch("DROP TRIGGER temp.no_events;")
        .unwrap();
    assert!(control.set_paused(false).await);
    assert_eq!(recorded(&[PAUSE, RESUME]).map(|e| e.kind), Some(RESUME));
    pipeline.shutdown().await.unwrap();
}

/// Emits what the test sends it, as a discovery source.
struct Scripted {
    manifest: Manifest,
    rx: Mutex<Option<tokio::sync::mpsc::Receiver<lodestar_sources::Found>>>,
}

impl SourcePlugin for Scripted {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        let mut rx = self.rx.lock().unwrap().take().expect("run once");
        Box::pin(async move {
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(f) = rx.recv() => {
                        ctx.emitter.emit(f).await.map_err(|_| PluginError("closed".into()))?;
                    }
                }
            }
        })
    }
}

/// Polls `done` every 25 ms for at most 20 s.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Lookups the pool held during a pause reach a DHT that has already
/// resumed: none is refused as paused, so none waits out a back-off. The
/// old pool-first resume lost this only in a narrow scheduling window, so
/// this guards the end-to-end behaviour; the DHT-first order in
/// `apply_paused` is what rules the race out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_sends_no_lookup_to_a_still_paused_dht() {
    resume_reaches_a_running_dht(false).await;
}

/// The same with coordinated admission (CR-ADM): the paused pool's keys
/// wait for permits instead, and resuming still asks only a running DHT,
/// so no permitted request is refused as paused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_coordinated_resume_sends_no_lookup_to_a_still_paused_dht() {
    resume_reaches_a_running_dht(true).await;
}

async fn resume_reaches_a_running_dht(coordinated: bool) {
    use lodestar_pipeline::DhtSource;
    use lodestar_sources::{Found, PoolConfig};
    const KEYS: u8 = 16;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let dht = Arc::new(DhtSource::new(lodestar_dht::node::Config {
        bind: vec!["127.0.0.1:0".parse().unwrap()],
        bootstrap: vec![],
        ..lodestar_dht::node::Config::default()
    }));
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let keys = Arc::new(Scripted {
        manifest: Manifest {
            id: "keys",
            name: "keys",
            version: "1",
            kinds: &[Kind::Discovery],
            networks: &[Network::Bt],
            endpoints: &[],
            risk: Risk::Low,
            consent: Consent::None,
            default_priority: Priority::Normal,
            enabled_by_default: true,
        },
        rx: Mutex::new(Some(rx)),
    });
    let config = PipelineConfig {
        pool: PoolConfig {
            lookup_wait: std::time::Duration::from_secs(600),
            retry_backoff: std::time::Duration::from_secs(3600),
            coordinated_admission: coordinated,
            finder_capacity: lodestar_dht::node::PEER_REQUEST_CAPACITY,
            ..PoolConfig::default()
        },
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::start_with(
        config,
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![dht.clone(), keys],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    let control = Control::start(
        pipeline.monitor(),
        Some(dht.clone()),
        Arc::new(Mutex::new(
            Store::open(&db, StoreConfig::default()).unwrap(),
        )),
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    until("the DHT runs", || dht.stats().is_some()).await;
    assert!(control.set_paused(true).await);
    for n in 1..=KEYS {
        tx.send(Found::key(InfoHash([n; 20]), 0)).await.unwrap();
    }
    let pool = pipeline.pool_stats();
    until("the paused pool holds every lookup", || {
        let held = if coordinated {
            &pool.admission_waiters
        } else {
            &pool.lookup_backlog
        };
        held.load(Ordering::Relaxed) == u64::from(KEYS)
    })
    .await;
    assert!(control.set_paused(false).await);
    // The DHT's stats refresh each second.
    until("every lookup admitted", || {
        dht.stats()
            .is_some_and(|s| s.peer_admitted >= u64::from(KEYS))
    })
    .await;
    let s = dht.stats().unwrap();
    assert_eq!(s.peer_rejected_paused, 0, "a lookup reached a paused DHT");
    assert_eq!(pool.lookup_unaccepted.load(Ordering::Relaxed), 0);
    assert_eq!(pool.admission_refusals[0].load(Ordering::Relaxed), 0);
    if coordinated {
        assert_eq!(s.peer_rejected, 0, "a permitted request was refused");
    }
    pipeline.shutdown().await.unwrap();
}
