//! Stored source states are enforced (a source that isn't enabled isn't
//! run), and the consent rule for enabling one (§18.1, §18.7).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_dht::node::Intensity;
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::control::{SourceChange, SourceError};
use lodestar_pipeline::{Control, Pipeline, PipelineConfig};
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
    // A Restricted source needs consent to its own class.
    let enable = |consent| SourceChange {
        enabled: Some(true),
        consent,
        ..SourceChange::default()
    };
    for consent in [None, Some(Consent::External), Some(Consent::Overlay)] {
        assert!(matches!(
            control.update_source(off, enable(consent)).await,
            Err(SourceError::ConsentRequired(Consent::Restricted))
        ));
    }
    let view = control
        .update_source(off, enable(Some(Consent::Restricted)))
        .await
        .unwrap();
    assert_eq!(view.state, SourceState::Enabled);
    assert!(!view.status.running, "it starts at the next start");
    // An External source is a toggle.
    let view = control.update_source(held, enable(None)).await.unwrap();
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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        off_ran.load(Ordering::SeqCst),
        "enabled with consent: it runs"
    );
    assert!(!held_ran.load(Ordering::SeqCst));
    let statuses = pipeline.sources();
    let held = statuses.iter().find(|s| s.plugin == "held").unwrap();
    assert_eq!(held.priority, Priority::Max, "the saved priority");
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
