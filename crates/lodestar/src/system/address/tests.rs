//! The keep-or-revert timer with an injected clock: recorded on save,
//! armed only once serving on the new address, kept, reverted (by the
//! deadline, at start, on a bind failure, or when asked), and never armed
//! by an address the environment sets.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use lodestar_api::settings::{
    Imported, Saved, SettingsError, SettingsFailure, SettingsImport, SettingsIndex, SettingsPatch,
    SettingsPreview,
};
use lodestar_api::system::RestartMethod;
use lodestar_store::StoreConfig;
use serde_json::json;

use super::*;

#[derive(Debug)]
struct FakeClock(AtomicU64);

impl FakeClock {
    fn at(t: u64) -> Arc<FakeClock> {
        Arc::new(FakeClock(AtomicU64::new(t)))
    }
    fn advance(&self, secs: u64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
    fn back(&self, secs: u64) {
        self.0.fetch_sub(secs, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

const T0: u64 = 1_800_000_000;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn local(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// A data directory whose `config.toml` binds `base`, and any extra
/// environment.
fn config(dir: &std::path::Path, base: SocketAddr, env: &[(&str, &str)]) -> Config {
    let file = dir.join("config.toml");
    std::fs::write(&file, format!("[http]\nbind = \"{base}\"\n")).unwrap();
    let mut vars: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    vars.push((
        "LODESTAR_DATA_DIR".into(),
        dir.join("data").display().to_string(),
    ));
    let config = Config::load(Some(&file), |k| {
        vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    })
    .unwrap();
    std::fs::create_dir_all(&config.data_dir).unwrap();
    config
}

fn store(config: &Config) -> Arc<Mutex<Store>> {
    Arc::new(Mutex::new(
        Store::open(config.db_path(), StoreConfig::default()).unwrap(),
    ))
}

fn registry(config: &Config) -> Arc<Registry> {
    Arc::new(Registry::load(config, store(config)).unwrap())
}

fn save_bind(registry: &Registry, value: Value) {
    let mut patch = Map::new();
    patch.insert("http_bind".into(), value);
    registry
        .save(SettingsBatch(BTreeMap::from([(
            "network".to_string(),
            SettingsPatch(patch),
        )])))
        .unwrap();
}

fn stored(config: &Config) -> Option<Record> {
    read(&store(config).lock().unwrap(), Kind::HttpBind)
}

fn put(config: &Config, record: &Record) {
    write(
        &mut store(config).lock().unwrap(),
        Kind::HttpBind,
        Some(record),
    );
}

/// The address and its source, as a freshly loaded registry sees them.
fn effective(config: &Config) -> (SocketAddr, SettingSource) {
    current(registry(config).as_ref()).unwrap()
}

struct Running {
    address: Arc<Address>,
    registry: Arc<Registry>,
    restart: Arc<Restart>,
    clock: Arc<FakeClock>,
}

/// A running server's timer: as `Services::start` builds it (prepare,
/// the registry, bind), without the rest of the server.
async fn start(base: &Config, clock: Arc<FakeClock>, method: RestartMethod) -> Running {
    let prepared = prepare(base, clock.as_ref()).await;
    let store = store(base);
    let registry = Arc::new(Registry::load(base, store.clone()).unwrap());
    let effective = registry.effective_config(base);
    let (_listener, bound) = bind(prepared, &effective, &store, clock.as_ref())
        .await
        .unwrap();
    start_with(bound, &effective, store, registry, clock, method)
}

/// [`start`]'s last step, with any `SettingsApi` in place of the registry
/// for the address's saves.
fn start_with(
    bound: Bound,
    effective: &Config,
    store: Arc<Mutex<Store>>,
    registry: Arc<Registry>,
    clock: Arc<FakeClock>,
    method: RestartMethod,
) -> Running {
    start_over(
        bound,
        effective,
        store,
        registry.clone(),
        registry,
        clock,
        method,
    )
}

fn start_over(
    bound: Bound,
    effective: &Config,
    store: Arc<Mutex<Store>>,
    settings: Arc<dyn SettingsApi>,
    registry: Arc<Registry>,
    clock: Arc<FakeClock>,
    method: RestartMethod,
) -> Running {
    let restart = Arc::new(Restart::new(method));
    let mut address = Address::new(
        bound,
        effective,
        store,
        settings,
        None,
        restart.clone(),
        clock.clone(),
    );
    // The timer reads the fake clock every second, not every 30.
    address.tick = 1;
    let address = Arc::new(address);
    Running {
        address,
        registry,
        restart,
        clock,
    }
}

fn pending(new: SocketAddr, previous: Previous, state: Pending, deadline: Option<u64>) -> Record {
    Record {
        kind: Kind::HttpBind,
        previous,
        new: json!(new.to_string()),
        state,
        deadline,
        serving: None,
    }
}

#[tokio::test]
async fn a_save_is_recorded_and_a_second_change_keeps_the_first_previous() {
    let dir = tempfile::tempdir().unwrap();
    let base = local(free_port());
    let config = config(dir.path(), base, &[]);
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    assert_eq!(run.address.change(), None);

    let (a, b) = (local(free_port()), local(free_port()));
    save_bind(&run.registry, json!(a.to_string()));
    run.address.saved();
    assert_eq!(
        stored(&config),
        Some(pending(a, Previous::Reset, Pending::AwaitingRestart, None))
    );
    let change = run.address.change().unwrap();
    assert_eq!(change.state, AddressState::AwaitingRestart);
    assert_eq!(change.url, format!("http://{a}/"));
    assert_eq!(change.previous_url, format!("http://{base}/"));
    assert_eq!(change.deadline, None);

    save_bind(&run.registry, json!(b.to_string()));
    run.address.saved();
    assert_eq!(
        stored(&config),
        Some(pending(b, Previous::Reset, Pending::AwaitingRestart, None))
    );
    // Back to the address that worked: nothing is pending.
    save_bind(&run.registry, Value::Null);
    run.address.saved();
    assert_eq!(stored(&config), None);
    assert_eq!(run.address.change(), None);
}

#[tokio::test]
async fn a_saved_previous_is_kept_as_saved() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let saved = local(free_port());
    save_bind(&registry(&config), json!(saved.to_string()));
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    let new = local(free_port());
    save_bind(&run.registry, json!(new.to_string()));
    run.address.saved();
    assert_eq!(
        stored(&config).unwrap().previous,
        Previous::Saved(json!(saved.to_string()))
    );
    // Revert before the restart: the saved value comes back, nothing
    // restarts and nothing is pending.
    let outcome = run.address.revert().unwrap();
    assert!(!outcome.restarting);
    assert_eq!(outcome.address_change, None);
    assert_eq!(effective(&config), (saved, SettingSource::Db));
    assert_eq!(stored(&config), None);
    assert!(!run.restart.was_requested());
    // The follower doesn't record the revert as a change of its own.
    run.address.saved();
    assert_eq!(stored(&config), None);
}

#[tokio::test]
async fn armed_only_once_serving_on_the_new_address_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let base = local(free_port());
    let config = config(dir.path(), base, &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    put(
        &config,
        &pending(new, Previous::Reset, Pending::AwaitingRestart, None),
    );
    // Awaiting the restart, there's nothing to keep.
    let clock = FakeClock::at(T0);
    let run = start(&config, clock.clone(), RestartMethod::Reexec).await;
    let change = run.address.change().unwrap();
    assert_eq!(change.state, AddressState::AwaitingConfirmation);
    assert_eq!(change.deadline, Some(T0 + WINDOW_SECS));
    assert_eq!(run.address.inner.lock().unwrap().serving, new);
    assert_eq!(
        stored(&config),
        Some(pending(
            new,
            Previous::Reset,
            Pending::AwaitingConfirmation,
            Some(T0 + WINDOW_SECS)
        ))
    );
    let kept = run.address.keep().unwrap();
    assert_eq!(kept.address_change, None);
    assert_eq!(stored(&config), None);
    assert_eq!(run.address.change(), None);
    // Kept is kept: the deadline passing changes nothing.
    run.clock.advance(WINDOW_SECS + 1);
    assert!(!run.address.expire());
    assert_eq!(effective(&config), (new, SettingSource::Db));
    assert_eq!(run.address.keep(), Err(SystemError::NoAddressChange));
    assert_eq!(run.address.revert(), Err(SystemError::NoAddressChange));
}

#[tokio::test]
async fn awaiting_the_restart_there_is_nothing_to_keep() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    save_bind(&run.registry, json!(local(free_port()).to_string()));
    run.address.saved();
    assert_eq!(run.address.keep(), Err(SystemError::NoAddressChange));
    // The deadline isn't running before the restart.
    run.clock.advance(WINDOW_SECS * 2);
    assert!(!run.address.expire());
}

/// §10.10.9: "the HTTP-address timer reverts when nobody confirms".
#[tokio::test]
async fn reverts_when_nobody_confirms() {
    let dir = tempfile::tempdir().unwrap();
    let base = local(free_port());
    let config = config(dir.path(), base, &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    put(
        &config,
        &pending(new, Previous::Reset, Pending::AwaitingRestart, None),
    );
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    run.clock.advance(WINDOW_SECS - 1);
    assert!(!run.address.expire());
    assert!(!run.restart.was_requested());
    run.clock.advance(1);
    assert!(run.address.expire());
    // The reset comes back (the file's address), the record is gone, and
    // the server restarts onto it.
    assert_eq!(effective(&config), (base, SettingSource::File));
    assert_eq!(stored(&config), None);
    assert!(run.restart.was_requested());
    let change = run.address.change().unwrap();
    assert_eq!(change.state, AddressState::Reverted);
    assert_eq!(change.url, format!("http://{new}/"));
    assert_eq!(change.previous_url, format!("http://{base}/"));
    // The next start binds the previous address and arms nothing.
    let next = start(&config, FakeClock::at(T0 + 1000), RestartMethod::Reexec).await;
    assert_eq!(next.address.change(), None);
    assert_eq!(next.address.inner.lock().unwrap().serving, base);
}

#[tokio::test]
async fn the_timer_task_reverts_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    // Armed with a deadline a second away.
    put(
        &config,
        &pending(
            new,
            Previous::Reset,
            Pending::AwaitingConfirmation,
            Some(T0 + 1),
        ),
    );
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    run.address.start(run.registry.subscribe());
    run.clock.advance(1);
    tokio::time::timeout(Duration::from_secs(5), run.restart.requested())
        .await
        .expect("the timer reverted and asked for a restart");
    assert_eq!(stored(&config), None);
}

#[tokio::test]
async fn where_it_cant_restart_the_previous_value_waits_for_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let (saved, new) = (local(free_port()), local(free_port()));
    save_bind(&registry(&config), json!(new.to_string()));
    put(
        &config,
        &pending(
            new,
            Previous::Saved(json!(saved.to_string())),
            Pending::AwaitingRestart,
            None,
        ),
    );
    let run = start(&config, FakeClock::at(T0), RestartMethod::Manual).await;
    run.clock.advance(WINDOW_SECS);
    assert!(run.address.expire());
    assert!(!run.restart.was_requested());
    assert_eq!(effective(&config), (saved, SettingSource::Db));
    // Still serving on the new address until then: a restart is needed.
    let network = run.registry.section("network").unwrap();
    assert_eq!(network.restart_required, vec!["http_bind".to_string()]);
    assert_eq!(run.address.change().unwrap().state, AddressState::Reverted);
}

#[tokio::test]
async fn revert_when_asked_restarts_onto_the_previous_address() {
    let dir = tempfile::tempdir().unwrap();
    let base = local(free_port());
    let config = config(dir.path(), base, &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    put(
        &config,
        &pending(new, Previous::Reset, Pending::AwaitingRestart, None),
    );
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    let outcome = run.address.revert().unwrap();
    // The route asks for the restart once its answer is out.
    assert!(outcome.restarting);
    assert!(!run.restart.was_requested());
    assert_eq!(
        outcome.address_change.unwrap().state,
        AddressState::Reverted
    );
    assert_eq!(effective(&config).0, base);
}

#[tokio::test]
async fn the_deadline_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    put(
        &config,
        &pending(new, Previous::Reset, Pending::AwaitingRestart, None),
    );
    let first = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    drop(first);
    // Restarted (or crashed) two minutes in: the same deadline.
    let again = start(&config, FakeClock::at(T0 + 120), RestartMethod::Reexec).await;
    assert_eq!(
        again.address.change().unwrap().deadline,
        Some(T0 + WINDOW_SECS)
    );
    assert_eq!(stored(&config).unwrap().deadline, Some(T0 + WINDOW_SECS));
}

#[tokio::test]
async fn a_deadline_already_past_at_start_reverts_before_binding() {
    let dir = tempfile::tempdir().unwrap();
    let base = local(free_port());
    let config = config(dir.path(), base, &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    put(
        &config,
        &pending(
            new,
            Previous::Reset,
            Pending::AwaitingConfirmation,
            Some(T0),
        ),
    );
    // Something else holds the new port: it must not even be tried.
    let _squatter = std::net::TcpListener::bind(new).unwrap();
    let run = start(&config, FakeClock::at(T0 + 1), RestartMethod::Reexec).await;
    let inner = run.address.inner.lock().unwrap();
    assert_eq!(inner.serving, base);
    assert_eq!(inner.reverted, Some((new, base)));
    drop(inner);
    assert_eq!(effective(&config), (base, SettingSource::File));
    assert_eq!(stored(&config), None);
    // Before the registry loaded: nothing waits for a restart.
    assert!(
        run.registry
            .section("network")
            .unwrap()
            .restart_required
            .is_empty()
    );
    assert!(!run.restart.was_requested());
}

#[tokio::test]
async fn a_new_address_that_fails_to_bind_reverts_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let (saved, new) = (local(free_port()), local(free_port()));
    save_bind(&registry(&config), json!(new.to_string()));
    put(
        &config,
        &pending(
            new,
            Previous::Saved(json!(saved.to_string())),
            Pending::AwaitingRestart,
            None,
        ),
    );
    let _squatter = std::net::TcpListener::bind(new).unwrap();
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    assert_eq!(run.address.inner.lock().unwrap().serving, saved);
    assert_eq!(effective(&config), (saved, SettingSource::Db));
    assert_eq!(stored(&config), None);
    assert_eq!(run.address.change().unwrap().state, AddressState::Reverted);
}

#[tokio::test]
async fn an_environment_bind_never_arms_it() {
    let dir = tempfile::tempdir().unwrap();
    let env = local(free_port());
    let env_text = env.to_string();
    let config = config(
        dir.path(),
        local(free_port()),
        &[("LODESTAR_HTTP_BIND", env_text.as_str())],
    );
    // A record from before the variable was set waits, untouched, for
    // the variable to go.
    let waiting = pending(
        local(free_port()),
        Previous::Reset,
        Pending::AwaitingConfirmation,
        Some(T0),
    );
    put(&config, &waiting);
    let run = start(&config, FakeClock::at(T0 + 1), RestartMethod::Reexec).await;
    assert_eq!(stored(&config), Some(waiting.clone()));
    assert_eq!(run.address.change(), None);
    assert_eq!(run.address.inner.lock().unwrap().serving, env);
    // The registry refuses the save, and the follower records nothing.
    let mut patch = Map::new();
    patch.insert("http_bind".into(), json!(local(free_port()).to_string()));
    assert!(
        run.registry
            .save(SettingsBatch(BTreeMap::from([(
                "network".to_string(),
                SettingsPatch(patch),
            )])))
            .is_err()
    );
    run.address.saved();
    assert_eq!(stored(&config), Some(waiting));
}

/// Recovered with `LODESTAR_HTTP_BIND`, then the variable removed: the
/// bad saved address still gets its timer, rather than binding with none.
#[tokio::test]
async fn removing_the_environment_bind_brings_the_timer_back() {
    let dir = tempfile::tempdir().unwrap();
    let base = local(free_port());
    let new = local(free_port());
    let rescue = local(free_port()).to_string();
    let with_env = config(dir.path(), base, &[("LODESTAR_HTTP_BIND", rescue.as_str())]);
    save_bind(
        &registry(&config(dir.path(), base, &[])),
        json!(new.to_string()),
    );
    put(
        &with_env,
        &pending(new, Previous::Reset, Pending::AwaitingRestart, None),
    );
    drop(start(&with_env, FakeClock::at(T0), RestartMethod::Reexec).await);
    let without = config(dir.path(), base, &[]);
    let run = start(&without, FakeClock::at(T0 + 10), RestartMethod::Reexec).await;
    let change = run.address.change().unwrap();
    assert_eq!(change.state, AddressState::AwaitingConfirmation);
    assert_eq!(change.deadline, Some(T0 + 10 + WINDOW_SECS));
}

#[test]
fn decisions_at_start() {
    let new = local(9000);
    let r = |state, deadline| pending(new, Previous::Reset, state, deadline);
    assert_eq!(
        decide(&r(Pending::AwaitingRestart, None), true, new, T0),
        Decision::Leave
    );
    // A deadline from a clock that has since gone back is never more than
    // a window away.
    assert_eq!(
        decide(
            &r(Pending::AwaitingConfirmation, Some(T0 + 86_400)),
            false,
            new,
            T0
        ),
        Decision::Bind {
            new,
            deadline: Some(T0 + WINDOW_SECS)
        }
    );
    assert!(matches!(
        decide(&r(Pending::AwaitingRestart, None), false, local(9001), T0),
        Decision::Clear(_)
    ));
    assert_eq!(
        decide(&r(Pending::AwaitingRestart, None), false, new, T0),
        Decision::Bind {
            new,
            deadline: None
        }
    );
    assert_eq!(
        decide(
            &r(Pending::AwaitingConfirmation, Some(T0 + 5)),
            false,
            new,
            T0
        ),
        Decision::Bind {
            new,
            deadline: Some(T0 + 5)
        }
    );
    assert_eq!(
        decide(&r(Pending::AwaitingConfirmation, Some(T0)), false, new, T0),
        Decision::Revert
    );
    // Crashed between binding and storing the deadline: it starts now.
    assert_eq!(
        decide(&r(Pending::AwaitingConfirmation, None), false, new, T0),
        Decision::Bind {
            new,
            deadline: None
        }
    );
}

#[test]
fn the_record_is_stored_outside_settings() {
    assert_eq!(Kind::HttpBind.key(), "system.keep_or_revert.http_bind");
    assert!(
        !Kind::HttpBind
            .key()
            .starts_with(crate::settings::KEY_PREFIX)
    );
    let text = serde_json::to_string(&pending(
        local(9000),
        Previous::Reset,
        Pending::AwaitingRestart,
        None,
    ))
    .unwrap();
    assert_eq!(
        text,
        r#"{"kind":"http_bind","previous":"reset","new":"127.0.0.1:9000","state":"awaiting_restart","deadline":null}"#
    );
    let saved = serde_json::to_value(Previous::Saved(json!("10.0.0.2:8090"))).unwrap();
    assert_eq!(saved, json!({ "saved": "10.0.0.2:8090" }));
}

#[tokio::test]
async fn the_follower_records_a_save_through_the_registry() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    run.address.start(run.registry.subscribe());
    let new = local(free_port());
    save_bind(&run.registry, json!(new.to_string()));
    let start = std::time::Instant::now();
    while stored(&config).is_none() {
        assert!(start.elapsed() < Duration::from_secs(5), "never recorded");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(stored(&config).unwrap().new, json!(new.to_string()));
    // Undone before the restart: the revert's own save isn't recorded as
    // a change by the running follower.
    run.address.revert().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(stored(&config), None);
    assert_eq!(run.address.change(), None);
}

#[tokio::test]
async fn a_clock_jump_past_the_deadline_still_reverts() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    // Booted with the clock a day behind (no RTC): the stored deadline
    // looks a day away.
    put(
        &config,
        &pending(
            new,
            Previous::Reset,
            Pending::AwaitingConfirmation,
            Some(T0 + 86_400),
        ),
    );
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    run.address.start(run.registry.subscribe());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!run.restart.was_requested());
    // NTP corrects it.
    run.clock.advance(86_400);
    tokio::time::timeout(Duration::from_secs(5), run.restart.requested())
        .await
        .expect("reverted within a tick of the jump");
}

/// The registry's reads, with every save failing.
#[derive(Debug)]
struct FailingSaves(Arc<Registry>);

impl SettingsApi for FailingSaves {
    fn index(&self) -> SettingsIndex {
        self.0.index()
    }
    fn section(&self, section: &str) -> Option<lodestar_api::settings::SectionValues> {
        self.0.section(section)
    }
    fn save(&self, _: SettingsBatch) -> Result<Saved, SettingsFailure> {
        Err(SettingsError::Store("disk I/O error".into()).into())
    }
    fn preview(&self, batch: SettingsBatch) -> Result<SettingsPreview, SettingsFailure> {
        self.0.preview(batch)
    }
    fn export(&self, preferences: bool) -> String {
        self.0.export(preferences)
    }
    fn import(&self, import: SettingsImport, dry_run: bool) -> Result<Imported, SettingsFailure> {
        self.0.import(import, dry_run)
    }
}

#[tokio::test]
async fn a_revert_that_cant_be_saved_restarts_so_the_start_reverts() {
    let dir = tempfile::tempdir().unwrap();
    let base = local(free_port());
    let config = config(dir.path(), base, &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    put(
        &config,
        &pending(new, Previous::Reset, Pending::AwaitingRestart, None),
    );
    let clock = FakeClock::at(T0);
    let prepared = prepare(&config, clock.as_ref()).await;
    let store = store(&config);
    let registry = Arc::new(Registry::load(&config, store.clone()).unwrap());
    let running = registry.effective_config(&config);
    let (listener, bound) = bind(prepared, &running, &store, clock.as_ref())
        .await
        .unwrap();
    let run = start_over(
        bound,
        &running,
        store,
        Arc::new(FailingSaves(registry.clone())),
        registry,
        clock,
        RestartMethod::Reexec,
    );
    run.clock.advance(WINDOW_SECS);
    assert!(!run.address.expire());
    // The record stays, past its deadline, and the server restarts...
    let left = stored(&config).unwrap();
    assert_eq!(left.deadline, Some(T0 + WINDOW_SECS));
    assert!(run.restart.was_requested());
    drop(listener);
    drop(run);
    // ...so the next start goes back before binding.
    let next = start(
        &config,
        FakeClock::at(T0 + WINDOW_SECS + 5),
        RestartMethod::Reexec,
    )
    .await;
    assert_eq!(next.address.inner.lock().unwrap().serving, base);
    assert_eq!(stored(&config), None);
    assert_eq!(effective(&config), (base, SettingSource::File));
}

#[tokio::test]
async fn a_clock_behind_at_start_clamps_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    // Stored under a clock a year ahead of this boot's.
    put(
        &config,
        &pending(
            new,
            Previous::Reset,
            Pending::AwaitingConfirmation,
            Some(T0 + 365 * 86_400),
        ),
    );
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    assert_eq!(
        run.address.change().unwrap().deadline,
        Some(T0 + WINDOW_SECS)
    );
    assert_eq!(stored(&config).unwrap().deadline, Some(T0 + WINDOW_SECS));
}

#[tokio::test]
async fn a_clock_that_goes_back_while_running_still_reverts() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let new = local(free_port());
    save_bind(&registry(&config), json!(new.to_string()));
    // A second of the window left.
    put(
        &config,
        &pending(
            new,
            Previous::Reset,
            Pending::AwaitingConfirmation,
            Some(T0 + 1),
        ),
    );
    let run = start(&config, FakeClock::at(T0), RestartMethod::Reexec).await;
    run.address.start(run.registry.subscribe());
    // NTP puts the clock a day back: the wall-clock deadline is now a day
    // away, but the window, measured on the monotonic clock, still ends.
    run.clock.back(86_400);
    tokio::time::timeout(Duration::from_secs(5), run.restart.requested())
        .await
        .expect("reverted when the monotonic window ran out");
    assert_eq!(stored(&config), None);
}

#[tokio::test]
async fn a_record_that_cant_be_stored_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), local(free_port()), &[]);
    let clock = FakeClock::at(T0);
    let prepared = prepare(&config, clock.as_ref()).await;
    let store = store(&config);
    let registry = Arc::new(Registry::load(&config, store.clone()).unwrap());
    let running = registry.effective_config(&config);
    let (_listener, bound) = bind(prepared, &running, &store, clock.as_ref())
        .await
        .unwrap();
    // Writes through a read-only connection fail.
    let read_only = Arc::new(Mutex::new(
        Store::open_reader(config.db_path(), StoreConfig::default()).unwrap(),
    ));
    let run = start_with(
        bound,
        &running,
        read_only,
        registry,
        clock,
        RestartMethod::Reexec,
    );
    save_bind(&run.registry, json!(local(free_port()).to_string()));
    run.address.saved();
    let change = run.address.change().unwrap();
    assert_eq!(change.state, AddressState::AwaitingRestart);
    assert!(!change.stored);
    assert_eq!(stored(&config), None);
}

#[test]
fn the_health_check_probes_the_address_served_after_a_failed_revert() {
    let dir = tempfile::tempdir().unwrap();
    let base = local(free_port());
    let config = config(dir.path(), base, &[]);
    let (new, previous) = (local(free_port()), local(free_port()));
    save_bind(&registry(&config), json!(new.to_string()));
    assert_eq!(health_bind(&config), new);
    let mut record = pending(
        new,
        Previous::Saved(json!(previous.to_string())),
        Pending::AwaitingConfirmation,
        Some(T0),
    );
    record.serving = Some(json!(previous.to_string()));
    put(&config, &record);
    assert_eq!(health_bind(&config), previous);
}
