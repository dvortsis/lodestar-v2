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
            always_adult: false,
            finds_private: false,
        },
        ran: ran.clone(),
    };
    (Arc::new(p), ran)
}

/// What [`Probe`] and [`Ticker`] declare: a required `api_key`, and 17
/// optional names so the 16-credential cap can still be reached through
/// declared names (X-consent-gate-lift r2 refuses undeclared ones).
static PROBE_CREDENTIALS: [lodestar_sources::consent::CredentialSpec; 18] = [
    lodestar_sources::consent::CredentialSpec {
        name: "api_key",
        label: "API key",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: true,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k0",
        label: "Key 0",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k1",
        label: "Key 1",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k2",
        label: "Key 2",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k3",
        label: "Key 3",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k4",
        label: "Key 4",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k5",
        label: "Key 5",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k6",
        label: "Key 6",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k7",
        label: "Key 7",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k8",
        label: "Key 8",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k9",
        label: "Key 9",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k10",
        label: "Key 10",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k11",
        label: "Key 11",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k12",
        label: "Key 12",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k13",
        label: "Key 13",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k14",
        label: "Key 14",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k15",
        label: "Key 15",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
    lodestar_sources::consent::CredentialSpec {
        name: "k16",
        label: "Key 16",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: false,
    },
];

static PROBE_POLICY: lodestar_sources::Policy = lodestar_sources::Policy {
    credentials: &PROBE_CREDENTIALS,
    ..lodestar_sources::Policy::STRICT
};

impl SourcePlugin for Probe {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn policy(&self) -> &lodestar_sources::Policy {
        &PROBE_POLICY
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
    until("the None source runs", || on_ran.load(Ordering::SeqCst)).await;
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
    // Restricted and Overlay open once the user accepts exactly the
    // current text version and addresses, with the password
    // (X-consent-gate-lift). External stays behind the gate in this
    // version. Every other request is refused, and a refusal writes
    // nothing.
    let enable = |consent, version: Option<String>, password_confirmed| SourceChange {
        enabled: Some(true),
        consent,
        consent_version: version,
        password_confirmed,
        ..SourceChange::default()
    };
    let fresh_id = pipeline
        .sources()
        .iter()
        .find(|s| s.plugin == "fresh")
        .unwrap()
        .id;
    let view_of = |control: &Control, id| {
        control
            .sources()
            .unwrap()
            .into_iter()
            .find(|v| v.status.id == id)
            .unwrap()
    };
    let gated = |r: &Result<_, SourceError>| {
        matches!(
            r,
            Err(SourceError::Invalid { field: "enabled", reason })
                if reason == lodestar_pipeline::control::CONSENT_GATE_REASON
        )
    };
    {
        let m = probe("held", Consent::External).0.manifest.clone();
        let version = consent::version(&m);
        for (c, v) in [
            (None, None),
            (Some(Consent::External), Some(version.clone())),
        ] {
            for password in [false, true] {
                let r = control
                    .update_source(held, enable(c, v.clone(), password))
                    .await;
                assert!(gated(&r), "held: {c:?} {v:?} {password}: {r:?}");
            }
        }
        // A stored acknowledgement doesn't open it either.
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        store
            .enable_source(held, Some(&Ack::of(&m).encode()))
            .unwrap();
        store.set_source_state(held, SourceState::Paused).unwrap();
        drop(store);
        assert_eq!(view_of(&control, held).need, None);
        let r = control.update_source(held, enable(None, None, true)).await;
        assert!(gated(&r), "held with a record: {r:?}");
        assert_ne!(view_of(&control, held).state, SourceState::Enabled);
    }
    for (id, plugin, class) in [
        (off, "off", Consent::Restricted),
        (fresh_id, "fresh", Consent::Overlay),
    ] {
        let m = probe(plugin, class).0.manifest.clone();
        let version = consent::version(&m);
        let stale = version.replacen("t2", "t1", 1);
        assert_ne!(stale, version);
        for (c, v) in [
            (None, None),
            (Some(class), None),
            (Some(class), Some("0-0000000000000000".to_owned())),
            (Some(class), Some(stale.clone())),
            (Some(class), Some(String::new())),
            (Some(Consent::None), Some(version.clone())),
            (Some(Consent::Account), Some(version.clone())),
        ] {
            for password in [false, true] {
                let r = control
                    .update_source(
                        id,
                        SourceChange {
                            priority: Some(Priority::Max),
                            ..enable(c, v.clone(), password)
                        },
                    )
                    .await;
                assert!(
                    matches!(&r, Err(SourceError::ConsentRequired { class: k, version: w })
                        if *k == class && *w == version),
                    "{plugin}: {c:?} {v:?} {password}: {r:?}"
                );
            }
        }
        if consent::needs_password(class) {
            let r = control
                .update_source(id, enable(Some(class), Some(version.clone()), false))
                .await;
            assert!(
                matches!(r, Err(SourceError::PasswordRequired)),
                "{plugin}: {r:?}"
            );
        }
        // Nothing was saved by any of those.
        let view = view_of(&control, id);
        assert_ne!(view.state, SourceState::Enabled, "{plugin}");
        assert_eq!(view.priority, Priority::Low, "{plugin}");
        assert!(view.need.is_some(), "{plugin}: still asks");
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        assert_eq!(store.source_consent(plugin).unwrap(), None, "{plugin}");
        drop(store);
        // Accepted: the exact version, and the password where it's asked.
        let view = control
            .update_source(id, enable(Some(class), Some(version.clone()), true))
            .await
            .unwrap();
        assert_eq!(view.state, SourceState::Enabled, "{plugin}");
        assert_eq!(view.need, None, "{plugin}");
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        assert_eq!(
            store.source_consent(plugin).unwrap(),
            Some(Ack::of(&m).encode()),
            "{plugin}: the record is the class, the version shown and accepted"
        );
    }
    // Turned off, then on again: the stored acknowledgement covers the
    // text, so it asks only for the password.
    for (id, class) in [(off, Consent::Restricted), (fresh_id, Consent::Overlay)] {
        control
            .update_source(
                id,
                SourceChange {
                    enabled: Some(false),
                    ..SourceChange::default()
                },
            )
            .await
            .unwrap();
        let r = control.update_source(id, enable(None, None, false)).await;
        if consent::needs_password(class) {
            assert!(matches!(r, Err(SourceError::PasswordRequired)), "{r:?}");
            let view = control
                .update_source(id, enable(None, None, true))
                .await
                .unwrap();
            assert_eq!(view.state, SourceState::Enabled);
        } else {
            assert_eq!(r.unwrap().state, SourceState::Enabled);
        }
    }
    // Disabling needs no consent, even for a gated source a build before
    // this one left on; an unknown id is refused.
    Store::open(&db, StoreConfig::default())
        .unwrap()
        .enable_source(held, None)
        .unwrap();
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

    // At the next start the stored states, the acceptance and the saved
    // priority hold: the accepted Restricted source runs, the one turned
    // off doesn't.
    let (on, on_ran) = probe_with("on", Consent::None, true);
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
    until("the None source runs", || on_ran.load(Ordering::SeqCst)).await;
    until("the accepted source runs", || {
        off_ran.load(Ordering::SeqCst)
    })
    .await;
    assert!(!held_ran.load(Ordering::SeqCst), "turned off");
    let statuses = pipeline.sources();
    let by = |id: &str| statuses.iter().find(|s| s.plugin == id).unwrap();
    assert!(by("off").enabled);
    assert_eq!(by("held").priority, Priority::Max, "the saved priority");
    assert!(!by("held").enabled);
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
        ("ext_current", Consent::External),
        ("acct_ready", Consent::Account),
        ("draft", Consent::Overlay),
        ("moved", Consent::Restricted),
        ("rereg", Consent::Overlay),
    ];
    // Kept on: an accepted acknowledgement of exactly what's shown now, of
    // a class the gate passes. `ext_current` is External, which stays
    // behind the gate in this version.
    let kept_on = ["on", "current", "rereg"];
    let acks = {
        // Enabled some other way (an older build, a hand edit, an import
        // that registered it): no record, or a stale one.
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        for (id, _) in classes {
            // `rereg` was registered by an older plugin version: the new
            // version re-registers it, and its acceptance still holds
            // (consent covers the class, text and addresses, not the
            // plugin's version).
            let version = if id == "rereg" { "0" } else { "1" };
            store.register_source(id, version, 0).unwrap();
        }
        let row = |store: &Store, plugin: &str| {
            store
                .sources()
                .unwrap()
                .into_iter()
                .find(|r| r.plugin == plugin)
                .unwrap()
                .id
        };
        let ack = |plugin, class| Ack::of(&probe(plugin, class).0.manifest);
        let stale = Ack {
            version: "0-0000000000000000".into(),
            ..ack("restr", Consent::Restricted)
        };
        let id = row(&store, "restr");
        store.enable_source(id, Some(&stale.encode())).unwrap();
        let mut acks = Vec::new();
        for (plugin, class) in [
            ("current", Consent::Restricted),
            ("ext_current", Consent::External),
            ("rereg", Consent::Overlay),
        ] {
            let id = row(&store, plugin);
            let a = ack(plugin, class).encode();
            store.enable_source(id, Some(&a)).unwrap();
            acks.push((plugin, a));
        }
        // An acceptance of the text before this one (SRC-4's draft, the
        // only text a build before the gate could record): the text
        // version moved on, so it's switched off until the new one is
        // accepted. The record is kept.
        let draft = Ack {
            version: ack("draft", Consent::Overlay)
                .version
                .replacen("t2", "t1", 1),
            ..ack("draft", Consent::Overlay)
        };
        assert!(draft.version.starts_with("t1"));
        let id = row(&store, "draft");
        store.enable_source(id, Some(&draft.encode())).unwrap();
        acks.push(("draft", draft.encode()));
        // An acceptance of another address list: the plugin contacts
        // something it didn't when it was accepted.
        let moved = Ack {
            version: format!("t2 {}:https://old.example", "https://old.example".len()),
            ..ack("moved", Consent::Restricted)
        };
        let id = row(&store, "moved");
        store.enable_source(id, Some(&moved.encode())).unwrap();
        // An Account source with its credentials is switched off: its
        // dialog is X-consent-gate-lift r2's. Its credentials are kept.
        let ready = row(&store, "acct_ready");
        store
            .update_source_secrets(ready, &[("api_key", b"sealed")], &[], None)
            .unwrap();
        store.enable_source(ready, None).unwrap();
        // A declined one counts as none.
        let over = row(&store, "over");
        let declined = Ack {
            accepted: false,
            ..ack("over", Consent::Overlay)
        };
        store.enable_source(over, Some(&declined.encode())).unwrap();
        // SOCKS5 credentials alone don't make an Account source ready.
        let acct = row(&store, "acct");
        store
            .update_source_secrets(acct, &[(consent::SOCKS5_USERNAME, b"sealed")], &[], None)
            .unwrap();
        acks
    };
    let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
    let mut ran = Vec::new();
    for (id, class) in classes {
        let (p, r) = probe(id, class);
        plugins.push(p);
        ran.push((id, r));
    }
    // A None source, on by default, shows the start has run its sources.
    let (on, on_ran) = probe_with("on", Consent::None, true);
    plugins.push(on);
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        plugins,
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    until("the None source runs", || on_ran.load(Ordering::SeqCst)).await;
    until("the accepted sources run", || {
        ran.iter()
            .filter(|(id, _)| kept_on.contains(id))
            .all(|(_, r)| r.load(Ordering::SeqCst))
    })
    .await;
    for s in pipeline.sources() {
        assert_eq!(s.enabled, kept_on.contains(&s.plugin), "{}", s.plugin);
    }
    for (id, r) in &ran {
        if !kept_on.contains(id) {
            assert!(!r.load(Ordering::SeqCst), "{id} ran");
        }
    }
    drop(pipeline.shutdown().await.unwrap());
    for (id, _) in classes {
        let want = if kept_on.contains(&id) {
            SourceState::Enabled
        } else {
            SourceState::Disabled
        };
        assert_eq!(stored(&db, id), want, "{id}");
    }
    // The acknowledgements and credentials are kept.
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    for (plugin, a) in acks {
        assert_eq!(store.source_consent(plugin).unwrap(), Some(a), "{plugin}");
    }
    assert_eq!(
        store.source_secret_names("acct_ready").unwrap(),
        ["api_key"]
    );
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
    // Account's credentials dialog isn't approved text yet (§2.49,
    // X-consent-gate-polish): enabling is refused before credentials are
    // looked at, with or without them. The source still says it needs
    // them.
    let gated = |r: Result<_, SourceError>| {
        matches!(
            r,
            Err(SourceError::Invalid { field: "enabled", ref reason })
                if reason == lodestar_pipeline::control::CONSENT_GATE_REASON
        )
    };
    assert!(gated(control.update_source(id, on.clone()).await));
    let need = |control: &Control| {
        control
            .sources()
            .unwrap()
            .into_iter()
            .find(|v| v.status.id == id)
            .unwrap()
            .need
    };
    assert_eq!(need(&control), Some(consent::Need::Credentials));
    // A bad name is refused, naming no value.
    let bad = control
        .update_credentials(
            id,
            &creds,
            vec![("Bad Name".into(), Some(Secret::new("v")))],
        )
        .await;
    assert!(matches!(bad, Err(SourceError::Credential(_))));
    // A name the plugin doesn't declare is refused too, and nothing is
    // stored: a typo can't stand in for `api_key` (X-consent-gate-lift r2).
    for typo in ["apikey", "api_key_", "token"] {
        let r = control
            .update_credentials(id, &creds, vec![(typo.into(), Some(Secret::new("v")))])
            .await;
        assert!(
            matches!(
                r,
                Err(SourceError::Credential(
                    consent::CredentialRefusal::Undeclared
                ))
            ),
            "{typo}: {r:?}"
        );
    }
    assert!(
        store
            .lock()
            .unwrap()
            .source_secret_names("acct")
            .unwrap()
            .is_empty()
    );
    // Removing an undeclared name is allowed (one an older build stored).
    control
        .update_credentials(id, &creds, vec![("token".into(), None)])
        .await
        .unwrap();
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
    assert_eq!(need(&control), None);
    assert!(gated(control.update_source(id, on).await));
    assert_eq!(stored(&db, "acct"), SourceState::Disabled);
    // One a build before the gate turned on (seeded here, as the API no
    // longer can): removing its credential disables it in the same write.
    store.lock().unwrap().enable_source(id, None).unwrap();
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
            always_adult: false,
            finds_private: false,
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

/// Fetches its one endpoint through its kit once, and records the result.
struct Fetches {
    manifest: Manifest,
    result: Arc<Mutex<Option<Result<u16, lodestar_sources::HttpError>>>>,
}

impl SourcePlugin for Fetches {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let r = ctx
                .kit
                .http
                .send(&lodestar_sources::Request::get("https://feed.example/rss"))
                .await
                .map(|r| r.status);
            *self.result.lock().unwrap() = Some(r);
            ctx.shutdown.requested().await;
            Ok(())
        })
    }
}

/// A source's saved SOCKS5 route is what its plugin's requests take: with
/// the proxy down, the kill switch refuses them (as its own error, so the
/// host isn't backed off), and the Sources view and its counters say so.
/// Changing the route needs the password; agents and the schedule are
/// checked against the plugin and saved.
#[tokio::test]
async fn a_source_runs_on_its_saved_egress_and_its_settings_are_checked() {
    use lodestar_pipeline::control::{EgressSetting, egress_warnings};
    use lodestar_sources::{
        DownReason, Egress, EgressFactory, Endpoint, ProxySpec, RouteState, SocksDns, Window,
    };
    use lodestar_store::SourceSetting;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    // A port nothing listens on: the stand-in for a VPN proxy that's down.
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let saved = EgressSetting::Socks5(ProxySpec {
        host: "127.0.0.1".into(),
        port: dead.port(),
        dns: None,
    });
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let id = store
        .register_source_as("feed", "1", SourceState::Enabled, 0)
        .unwrap();
    store
        .set_source_settings(id, &[(SourceSetting::Egress, Some(&saved.encode()))])
        .unwrap();
    drop(store);

    let result = Arc::new(Mutex::new(None));
    let plugin = Arc::new(Fetches {
        manifest: Manifest {
            id: "feed",
            name: "Feed",
            version: "1",
            kinds: &[Kind::Discovery],
            networks: &[Network::Bt],
            endpoints: &[Endpoint {
                protocol: "https",
                host: "feed.example",
            }],
            risk: Risk::Low,
            consent: Consent::None,
            default_priority: Priority::Normal,
            enabled_by_default: true,
            always_adult: false,
            finds_private: false,
        },
        result: result.clone(),
    });
    let config = PipelineConfig {
        egress: Some(EgressFactory::new(Arc::new(
            lodestar_sources::egress::NoCredentials,
        ))),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::start_with(
        config,
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![plugin],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    until("the plugin's request", || result.lock().unwrap().is_some()).await;
    let got = result.lock().unwrap().take().unwrap();
    assert!(
        matches!(got, Err(lodestar_sources::HttpError::EgressDown(_))),
        "{got:?}"
    );
    let status = pipeline.sources().into_iter().next().unwrap();
    let live = status.egress.clone().unwrap();
    assert_eq!(
        live.status.state,
        RouteState::Down(DownReason::ProxyUnreachable)
    );
    assert_eq!(live.status.counters.kill_switch_refusals, 1);
    assert_eq!(live.status.counters.answered, 0);
    // Left on `auto`, a plugin pinned to its manifest's hosts lets the
    // proxy resolve names, wherever the proxy is (runner, #394).
    assert_eq!(
        live.route,
        Some(Egress::Socks5 {
            proxy: dead,
            dns: SocksDns::Proxy
        })
    );
    assert_eq!(live.setting, Ok(saved.clone()));
    // Proxy DNS on a proxy on this machine: the trade is named.
    assert_eq!(
        egress_warnings(&Ok(saved.clone()), Some(&live), false, SocksDns::Proxy),
        [lodestar_pipeline::control::WARN_LAN_PROXY_DNS]
    );

    let control = Control::start(
        pipeline.monitor(),
        None,
        Arc::new(Mutex::new(
            Store::open(&db, StoreConfig::default()).unwrap(),
        )),
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    let view = control.sources().unwrap().into_iter().next().unwrap();
    assert_eq!(view.settings.egress, Ok(saved));
    let direct = SourceChange {
        egress: Some(EgressSetting::Direct),
        ..SourceChange::default()
    };
    assert!(matches!(
        control.update_source(id, direct.clone()).await,
        Err(SourceError::PasswordRequired)
    ));
    let view = control
        .update_source(
            id,
            SourceChange {
                password_confirmed: true,
                ..direct
            },
        )
        .await
        .unwrap();
    assert_eq!(view.settings.egress, Ok(EgressSetting::Direct));
    // The plugin declares one agent at most (the strict default).
    assert!(matches!(
        control
            .update_source(
                id,
                SourceChange {
                    agents: Some(2),
                    ..SourceChange::default()
                },
            )
            .await,
        Err(SourceError::Invalid {
            field: "agents",
            ..
        })
    ));
    let view = control
        .update_source(
            id,
            SourceChange {
                schedule: Some(vec![Window::new(60, 420)]),
                agents: Some(1),
                ..SourceChange::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(view.settings.schedule, Some(vec![Window::new(60, 420)]));
    assert_eq!(view.settings.agents, Some(1));
    pipeline.shutdown().await.unwrap();
}

/// An egress setting this build can't read fails closed (runner, #394):
/// the plugin runs with its route down for good, every request is refused,
/// and the view says the setting is unreadable instead of direct. A
/// schedule that can't be read falls back to its default.
#[tokio::test]
async fn an_unreadable_egress_setting_refuses_every_request() {
    use lodestar_pipeline::control::{Unreadable, WARN_SETTING_UNREADABLE, egress_warnings};
    use lodestar_sources::{DownReason, EgressFactory, Endpoint, RouteState, SocksDns};
    use lodestar_store::SourceSetting;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    // A route a later build might add, and a damaged SOCKS5 value.
    let rows = [
        ("newer", "interface wg0"),
        ("damaged", "socks5 10.8.0.1 1080"),
    ];
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    for (plugin, text) in rows {
        let id = store
            .register_source_as(plugin, "1", SourceState::Enabled, 0)
            .unwrap();
        store
            .set_source_settings(
                id,
                &[
                    (SourceSetting::Egress, Some(text)),
                    (SourceSetting::Schedule, Some("not a schedule")),
                ],
            )
            .unwrap();
    }
    drop(store);
    let results: Vec<_> = rows.iter().map(|_| Arc::new(Mutex::new(None))).collect();
    let plugins: Vec<Arc<dyn SourcePlugin>> = rows
        .iter()
        .zip(&results)
        .map(|((id, _), result)| {
            Arc::new(Fetches {
                manifest: Manifest {
                    id,
                    name: id,
                    version: "1",
                    kinds: &[Kind::Discovery],
                    networks: &[Network::Bt],
                    endpoints: &[Endpoint {
                        protocol: "https",
                        host: "feed.example",
                    }],
                    risk: Risk::Low,
                    consent: Consent::None,
                    default_priority: Priority::Normal,
                    enabled_by_default: true,
                    always_adult: false,
                    finds_private: false,
                },
                result: result.clone(),
            }) as Arc<dyn SourcePlugin>
        })
        .collect();
    let config = PipelineConfig {
        egress: Some(EgressFactory::new(Arc::new(
            lodestar_sources::egress::NoCredentials,
        ))),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::start_with(
        config,
        Store::open(&db, StoreConfig::default()).unwrap(),
        plugins,
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    for result in &results {
        until("the plugin's request", || result.lock().unwrap().is_some()).await;
        let got = result.lock().unwrap().take().unwrap();
        assert!(
            matches!(&got, Err(lodestar_sources::HttpError::EgressDown(e)) if e.contains("couldn't be read")),
            "{got:?}"
        );
    }
    for status in pipeline.sources() {
        let live = status.egress.clone().unwrap();
        assert_eq!(
            live.status.state,
            RouteState::Down(DownReason::SettingUnreadable),
            "{}",
            status.plugin
        );
        assert_eq!(live.route, None);
        assert_eq!(live.setting, Err(Unreadable));
        assert_eq!(live.status.counters.kill_switch_refusals, 1);
    }
    let control = Control::start(
        pipeline.monitor(),
        None,
        Arc::new(Mutex::new(
            Store::open(&db, StoreConfig::default()).unwrap(),
        )),
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    for view in control.sources().unwrap() {
        assert_eq!(view.settings.egress, Err(Unreadable));
        assert_eq!(view.settings.schedule, None, "its default applies");
        assert_eq!(view.egress_warnings, [WARN_SETTING_UNREADABLE]);
    }
    assert_eq!(
        egress_warnings(&Err(Unreadable), None, true, SocksDns::Proxy),
        [WARN_SETTING_UNREADABLE]
    );
    pipeline.shutdown().await.unwrap();
}

/// A purge stops its source's plugin at once, survives a crash partway
/// (the source stays off, what was done stays done, and enabling waits
/// for the purge), and finishes when it's run again (SRC-14b).
#[tokio::test]
async fn a_purge_stops_the_source_survives_a_crash_and_finishes_when_run_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let start = |store| async {
        let (p, ran) = probe_with("nyaa", Consent::None, true);
        let pipeline = Pipeline::start_with(
            PipelineConfig::default(),
            store,
            vec![p as Arc<dyn SourcePlugin>],
            Arc::new(NoFetch),
        )
        .await
        .unwrap();
        let control = Control::start(
            pipeline.monitor(),
            None,
            Arc::new(Mutex::new(
                Store::open(&db, StoreConfig::default()).unwrap(),
            )),
            false,
            Intensity::Balanced,
        )
        .await
        .unwrap();
        (pipeline, control, ran)
    };
    let (pipeline, control, ran) = start(Store::open(&db, StoreConfig::default()).unwrap()).await;
    until("the source runs", || ran.load(Ordering::SeqCst)).await;
    let id = pipeline.sources()[0].id;
    {
        let store = control.store();
        let mut store = store.lock().unwrap();
        store
            .update_source_secrets(id, &[("api_key", b"sealed")], &[], None)
            .unwrap();
    }
    // Only with the password.
    assert!(matches!(
        control.begin_purge(id, false).await,
        Err(SourceError::PasswordRequired)
    ));
    assert!(pipeline.sources()[0].running);
    control.begin_purge(id, true).await.unwrap();
    // Its plugin stopped at once, not at the next restart.
    assert!(!pipeline.sources()[0].running);
    // The "crash": nothing more of the purge runs.
    drop(control);
    drop(pipeline.shutdown().await.unwrap());

    let check = Store::open(&db, StoreConfig::default()).unwrap();
    assert_eq!(stored(&db, "nyaa"), SourceState::Disabled);
    assert!(check.source_purge_started("nyaa").unwrap().is_some());
    assert_eq!(
        check.source_secret_names("nyaa").unwrap(),
        ["api_key"],
        "not finished"
    );
    drop(check);

    // After a restart it stays off, says so, and can't be enabled yet.
    let (pipeline, control, ran) = start(Store::open(&db, StoreConfig::default()).unwrap()).await;
    assert!(!ran.load(Ordering::SeqCst));
    assert!(control.sources().unwrap()[0].purge_pending);
    let enable = SourceChange {
        enabled: Some(true),
        ..SourceChange::default()
    };
    assert!(matches!(
        control.update_source(id, enable.clone()).await,
        Err(SourceError::Invalid {
            field: "enabled",
            ..
        })
    ));
    // Run again, it finishes: everything of the source's is gone.
    control.begin_purge(id, true).await.unwrap();
    let mut cursor = Some(lodestar_store::unverified::PurgeCursor::START);
    while let Some(from) = cursor {
        cursor = control.purge_step(id, from).await.unwrap().next;
    }
    control.finish_purge(id).await.unwrap();
    let view = &control.sources().unwrap()[0];
    assert!(!view.purge_pending);
    assert_eq!(view.state, SourceState::Purged);
    assert!(view.credentials.is_empty());
    // Purged is a fresh, disabled source: it can be enabled again.
    control.update_source(id, enable).await.unwrap();
    drop(control);
    pipeline.shutdown().await.unwrap();
}

/// Takes any settings, and counts the checks that reached it.
struct Settable {
    manifest: Manifest,
    checks: Arc<std::sync::atomic::AtomicUsize>,
}

impl SourcePlugin for Settable {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn config_editable(&self) -> bool {
        true
    }

    fn config_json(&self) -> Option<String> {
        Some("{}".into())
    }

    fn check_config(&self, json: &str) -> Result<String, lodestar_sources::ConfigRefusal> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        Ok(json.to_owned())
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            ctx.shutdown.requested().await;
            Ok(())
        })
    }
}

/// SRC-13c: the control's own size cap holds before the plugin is asked,
/// a plugin whose settings can't be written refuses them, and what's saved
/// is pending until the next start.
#[tokio::test]
async fn settings_are_capped_checked_by_the_plugin_and_pending_until_a_start() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let settable = Arc::new(Settable {
        manifest: Manifest {
            id: "settable",
            ..probe("settable", Consent::None).0.manifest.clone()
        },
        checks: checks.clone(),
    });
    let (plain, _) = probe("plain", Consent::None);
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![settable, plain],
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
        control_store,
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    let id = |plugin: &str| {
        pipeline
            .sources()
            .into_iter()
            .find(|s| s.plugin == plugin)
            .unwrap()
            .id
    };
    let config = |json: String| SourceChange {
        config: Some(json),
        ..SourceChange::default()
    };
    let big = format!(
        "{{\"x\":\"{}\"}}",
        "a".repeat(lodestar_pipeline::control::MAX_CONFIG_BYTES)
    );
    assert!(matches!(
        control.update_source(id("settable"), config(big)).await,
        Err(SourceError::Invalid {
            field: "config",
            ..
        })
    ));
    assert_eq!(
        checks.load(Ordering::SeqCst),
        0,
        "refused before the plugin"
    );
    assert!(matches!(
        control
            .update_source(id("plain"), config("{}".into()))
            .await,
        Err(SourceError::Invalid {
            field: "config",
            ..
        })
    ));
    let view = control
        .update_source(id("settable"), config(r#"{"a":1}"#.into()))
        .await
        .unwrap();
    assert!(view.config_pending, "applies at the next start");
    // The plugin is asked once for the write and once for the view.
    assert!(checks.load(Ordering::SeqCst) >= 1);
    let same = control
        .update_source(id("settable"), config("{}".into()))
        .await
        .unwrap();
    assert!(!same.config_pending, "the running settings, saved again");
    pipeline.shutdown().await.unwrap();
}

/// Sends one "request" (a count) every 10 ms while it runs: the stand-in
/// for a plugin whose lookups go out until it's stopped.
struct Ticker {
    manifest: Manifest,
    sent: Arc<std::sync::atomic::AtomicU64>,
}

impl SourcePlugin for Ticker {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn policy(&self) -> &lodestar_sources::Policy {
        &PROBE_POLICY
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        let sent = self.sent.clone();
        Box::pin(async move {
            loop {
                tokio::select! {
                    () = ctx.shutdown.requested() => return Ok(()),
                    () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        sent.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        })
    }
}

fn ticker(id: &'static str, consent: Consent) -> (Arc<Ticker>, Arc<std::sync::atomic::AtomicU64>) {
    let sent = Arc::default();
    let (p, _) = probe_with(id, consent, true);
    (
        Arc::new(Ticker {
            manifest: p.manifest.clone(),
            sent: Arc::clone(&sent),
        }),
        sent,
    )
}

async fn started(
    db: &std::path::Path,
    plugin: Arc<dyn SourcePlugin>,
) -> (Pipeline, Control, Arc<Mutex<Store>>) {
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(db, StoreConfig::default()).unwrap(),
        vec![plugin],
        Arc::new(NoFetch),
    )
    .await
    .unwrap();
    let store = Arc::new(Mutex::new(Store::open(db, StoreConfig::default()).unwrap()));
    let control = Control::start(
        pipeline.monitor(),
        None,
        store.clone(),
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    (pipeline, control, store)
}

/// Turning a running source off stops its plugin at once, not at the next
/// start (X-tcache-recheck, superseding "applies at the next start" for
/// disabling): once the change returns it has ended, so nothing more goes
/// out. Turning it back on isn't blocked by that stop, and it runs again
/// from the next start, as every enable does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turning_a_running_source_off_stops_it_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (p, sent) = ticker("nyaa", Consent::None);
    let (pipeline, control, _) = started(&db, p).await;
    until("it sends", || sent.load(Ordering::SeqCst) >= 3).await;
    let id = pipeline.sources()[0].id;
    let off = SourceChange {
        enabled: Some(false),
        ..SourceChange::default()
    };
    let view = control.update_source(id, off).await.unwrap();
    assert_eq!(view.state, SourceState::Disabled);
    // Ended by the time the change returned: its `run` has returned, so
    // nothing more can go out (no wait needed to show it).
    assert!(!pipeline.sources()[0].running, "still running after off");

    // On again: allowed, and it runs from the next start.
    let on = SourceChange {
        enabled: Some(true),
        ..SourceChange::default()
    };
    let view = control.update_source(id, on).await.unwrap();
    assert_eq!(view.state, SourceState::Enabled);
    // It says it was stopped since this start, so the API can show it
    // runs only from the next one.
    assert!(view.stopped && !view.status.running, "{view:?}");
    drop(control);
    pipeline.shutdown().await.unwrap();
    let (p, sent) = ticker("nyaa", Consent::None);
    let (pipeline, _control, _) = started(&db, p).await;
    until("it sends again after the start", || {
        sent.load(Ordering::SeqCst) >= 1
    })
    .await;
    pipeline.shutdown().await.unwrap();
}

/// Removing the credential an enabled Account source needs disables it
/// and stops its running plugin at once (a key read once at start, as
/// Prowlarr's is, isn't used again).
///
/// Until Account's credentials dialog ships approved text (§2.49,
/// X-consent-gate-polish), no Account source can run: enabling one is
/// refused, and one a build before the gate turned on is switched off at
/// the start before it runs. X-consent-gate-lift brings back the running
/// leg when it opens Account.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_source_never_runs_and_losing_its_credential_keeps_it_off() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    {
        let (p, _) = ticker("acct", Consent::Account);
        let (pipeline, control, store) = started(&db, p).await;
        let id = pipeline.sources()[0].id;
        let creds =
            SourceCredentials::with_keys(store.clone(), Arc::new(SecretStore::from_key(&[3; 32])));
        control
            .update_credentials(
                id,
                &creds,
                vec![("api_key".into(), Some(Secret::new("the-key-value")))],
            )
            .await
            .unwrap();
        let on = SourceChange {
            enabled: Some(true),
            consent: Some(Consent::Account),
            ..SourceChange::default()
        };
        assert!(matches!(
            control.update_source(id, on).await,
            Err(SourceError::Invalid {
                field: "enabled",
                ..
            })
        ));
        // As a build before the gate could have left it.
        store.lock().unwrap().enable_source(id, None).unwrap();
        drop(control);
        pipeline.shutdown().await.unwrap();
    }
    let (p, sent) = ticker("acct", Consent::Account);
    let (pipeline, control, store) = started(&db, p).await;
    // The start switched it off before running it: `start_with` returns
    // once its sources are started, so nothing is still on its way.
    assert!(!pipeline.sources()[0].enabled);
    assert!(!pipeline.sources()[0].running);
    assert_eq!(sent.load(Ordering::SeqCst), 0, "it never ran");
    assert_eq!(stored(&db, "acct"), SourceState::Disabled);
    let id = pipeline.sources()[0].id;
    let creds = SourceCredentials::with_keys(store, Arc::new(SecretStore::from_key(&[3; 32])));
    let view = control
        .update_credentials(id, &creds, vec![("api_key".into(), None)])
        .await
        .unwrap();
    assert_eq!(view.state, SourceState::Disabled);
    assert!(
        !pipeline.sources()[0].running,
        "still running without its key"
    );
    pipeline.shutdown().await.unwrap();
}

/// Off, on and off again in one run: each change returns, the source ends
/// up off and stopped, and stopping a plugin that's already stopped is
/// harmless.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn off_on_off_leaves_the_source_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (p, sent) = ticker("nyaa", Consent::None);
    let (pipeline, control, _) = started(&db, p).await;
    until("it sends", || sent.load(Ordering::SeqCst) >= 1).await;
    let id = pipeline.sources()[0].id;
    let set = |enabled: bool| SourceChange {
        enabled: Some(enabled),
        ..SourceChange::default()
    };
    for (on, state) in [
        (false, SourceState::Disabled),
        (true, SourceState::Enabled),
        (false, SourceState::Disabled),
    ] {
        let view = control.update_source(id, set(on)).await.unwrap();
        assert_eq!(view.state, state);
        assert!(view.stopped, "{view:?}");
        assert!(!pipeline.sources()[0].running);
    }
    assert_eq!(stored(&db, "nyaa"), SourceState::Disabled);
    pipeline.shutdown().await.unwrap();
}

/// Reads `api_key` and takes any settings; `pins` says whether it keeps
/// the key it was built with (`SourcePlugin::pins_credentials`).
struct Keyed {
    manifest: Manifest,
    pins: bool,
}

const KEYED_POLICY: lodestar_sources::Policy = lodestar_sources::Policy {
    credentials: &[lodestar_sources::consent::CredentialSpec {
        name: "api_key",
        label: "API key",
        kind: lodestar_sources::consent::CredentialKind::Secret,
        required: true,
    }],
    ..lodestar_sources::Policy::STRICT
};

impl SourcePlugin for Keyed {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn policy(&self) -> &lodestar_sources::Policy {
        &KEYED_POLICY
    }

    fn config_editable(&self) -> bool {
        true
    }

    fn config_json(&self) -> Option<String> {
        Some("{}".into())
    }

    fn check_config(&self, json: &str) -> Result<String, lodestar_sources::ConfigRefusal> {
        Ok(json.to_owned())
    }

    fn pins_credentials(&self) -> bool {
        self.pins
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            ctx.shutdown.requested().await;
            Ok(())
        })
    }
}

/// X-restart-required-creds: a plugin that keeps the key it was built
/// with shows a key saved since as waiting for the next start
/// (`config_pending`, the API's `restart_required`), and the next start
/// clears it. One that reads its key fresh never shows it.
#[tokio::test]
async fn a_pinned_credential_saved_since_the_start_waits_for_a_restart() {
    const PLANTED: &str = "planted-fake-key-0123456789";
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let keys = Arc::new(SecretStore::from_key(&[7; 32]));
    let keyed = |id: &'static str, pins: bool| -> Arc<dyn SourcePlugin> {
        Arc::new(Keyed {
            manifest: Manifest {
                id,
                ..probe(id, Consent::Account).0.manifest.clone()
            },
            pins,
        })
    };
    let start = || async {
        let pipeline = Pipeline::start_with(
            PipelineConfig::default(),
            Store::open(&db, StoreConfig::default()).unwrap(),
            vec![keyed("pinning", true), keyed("fresh", false)],
            Arc::new(NoFetch),
        )
        .await
        .unwrap();
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
        let creds = SourceCredentials::with_keys(store, keys.clone());
        (pipeline, control, creds)
    };
    let id = |control: &Control, plugin: &str| {
        control
            .sources()
            .unwrap()
            .into_iter()
            .find(|v| v.status.plugin == plugin)
            .unwrap()
            .status
            .id
    };
    let pending = |control: &Control, plugin: &str| {
        let views = control.sources().unwrap();
        assert!(!format!("{views:?}").contains(PLANTED), "{views:?}");
        views
            .into_iter()
            .find(|v| v.status.plugin == plugin)
            .unwrap()
            .config_pending
    };
    let set_key =
        |control: &Control, creds: &SourceCredentials, plugin: &str, key: Option<&str>| {
            let (control, creds, id) = (control.clone(), creds.clone(), id(control, plugin));
            let key = key.map(Secret::new);
            async move {
                let view = control
                    .update_credentials(id, &creds, vec![("api_key".into(), key)])
                    .await
                    .unwrap();
                assert!(!format!("{view:?}").contains(PLANTED), "{view:?}");
                view.config_pending
            }
        };

    // A first key, stored after the start: the pinning plugin waits for
    // the next one; the fresh one reads it at its next run.
    let (pipeline, control, creds) = start().await;
    assert!(!pending(&control, "pinning") && !pending(&control, "fresh"));
    assert!(set_key(&control, &creds, "pinning", Some(PLANTED)).await);
    assert!(!set_key(&control, &creds, "fresh", Some(PLANTED)).await);
    assert!(!pending(&control, "fresh"), "a fresh reader never waits");
    assert!(!format!("{control:?}").contains(PLANTED));
    pipeline.shutdown().await.unwrap();
    drop(control);

    // The next start runs on it: nothing waits.
    let (pipeline, control, creds) = start().await;
    assert!(!pending(&control, "pinning"), "cleared by the restart");
    // A hand-over to the address it already has: the same settings saved
    // again, and a new key. The settings alone don't wait; the key does.
    let view = control
        .update_source(
            id(&control, "pinning"),
            SourceChange {
                config: Some("{}".into()),
                ..SourceChange::default()
            },
        )
        .await
        .unwrap();
    assert!(!view.config_pending, "the running settings, saved again");
    assert!(set_key(&control, &creds, "pinning", Some("companion-key-abc")).await);
    // Back to the start's value: it's another write, sealed afresh, so it
    // still waits, as a revision counter would.
    assert!(set_key(&control, &creds, "pinning", Some(PLANTED)).await);
    pipeline.shutdown().await.unwrap();
    drop(control);

    // Removed since the start: it waits too (it's running on a key it no
    // longer has). The fresh reader still never does. The first write
    // comes before any view is read, so the record is the start's, not
    // taken at the first read.
    let (pipeline, control, creds) = start().await;
    let pinning_id = pipeline
        .sources()
        .into_iter()
        .find(|s| s.plugin == "pinning")
        .unwrap()
        .id;
    let view = control
        .update_credentials(
            pinning_id,
            &creds,
            vec![("api_key".into(), Some(Secret::new(PLANTED)))],
        )
        .await
        .unwrap();
    assert!(view.config_pending, "a write before the first read counts");
    pipeline.shutdown().await.unwrap();
    drop(control);
    let (pipeline, control, creds) = start().await;
    assert!(!pending(&control, "pinning"));
    // Another source's write leaves it alone.
    assert!(!set_key(&control, &creds, "fresh", Some("other-key")).await);
    assert!(
        !pending(&control, "pinning"),
        "only its own credentials count"
    );
    assert!(set_key(&control, &creds, "pinning", None).await);
    assert!(!set_key(&control, &creds, "fresh", None).await);
    pipeline.shutdown().await.unwrap();
    drop(control);

    // Purged since the start: the purge stops it and drops its key with
    // its settings, so nothing waits for a restart.
    let (pipeline, control, creds) = start().await;
    assert!(set_key(&control, &creds, "pinning", Some(PLANTED)).await);
    pipeline.shutdown().await.unwrap();
    drop(control);
    let (pipeline, control, _creds) = start().await;
    let id = id(&control, "pinning");
    assert!(!pending(&control, "pinning"));
    control.begin_purge(id, true).await.unwrap();
    let mut cursor = Some(lodestar_store::unverified::PurgeCursor::START);
    while let Some(from) = cursor {
        cursor = control.purge_step(id, from).await.unwrap().next;
    }
    control.finish_purge(id).await.unwrap();
    let view = control
        .sources()
        .unwrap()
        .into_iter()
        .find(|v| v.status.id == id)
        .unwrap();
    assert!(view.credentials.is_empty(), "the purge dropped its key");
    assert!(!view.config_pending, "a purged source waits for nothing");
    pipeline.shutdown().await.unwrap();
}
