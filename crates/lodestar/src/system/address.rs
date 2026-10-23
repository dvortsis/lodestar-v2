//! The HTTP address's keep-or-revert timer (architecture §10.10.6
//! "Lockout protection", §10.10.9): a saved `network.http_bind` that
//! nobody confirms from the new address within 5 minutes goes back by
//! itself, the way a display-resolution change does.
//!
//! - **Recorded on save.** [`Address::follow`] watches the registry's
//!   `network` section. When a save changes `http_bind`, it writes a
//!   [`Record`]: the previous value (the saved one, or `reset` when
//!   nothing was saved), the new one, and `awaiting_restart`. A second
//!   change while one is pending keeps the original `previous`; a change
//!   back to `previous` drops the record. The record lives under
//!   `system.keep_or_revert.<kind>` in the store's settings table, outside
//!   `settings.`, so the registry never sees it and no migration is
//!   needed. An address the environment sets (`LODESTAR_HTTP_BIND`) can't
//!   be saved in the app, so it never arms the timer.
//! - **Armed at start.** [`prepare`] runs before the registry loads, and
//!   [`bind`] replaces the listener's bind. If the effective bind is the
//!   pending `new` one, the record becomes `awaiting_confirmation` with a
//!   deadline [`WINDOW_SECS`] after the listener binds. The deadline is
//!   stored, so a crash or restart doesn't reset it.
//! - **Kept** by `POST /system/address/keep`. Any request reaching this
//!   process reached it on the new address, whatever its `Host` header
//!   says (a reverse proxy rewrites it), so a signed-in session counts.
//! - **Reverted** when the deadline passes: the previous value is saved
//!   back through the registry (`null` for `reset`), logged, and the
//!   process restarts onto it. Where it can't restart itself, the value
//!   applies at the next restart and `/system` says so. A deadline
//!   already past at start reverts before binding, and a pending address
//!   that fails to bind reverts at once and binds the previous one, so a
//!   mistyped address can't crash-loop a container. Both happen before
//!   the registry loads, so nothing reports a restart as still needed.
//! - **Generic over what it reverts:** a record names its [`Kind`] and
//!   keeps values as JSON. Only `http_bind` is wired. A trusted-proxy
//!   mode that can lock out cookie sessions (a later security slice)
//!   would add a kind whose revert calls `Auth::set_proxy(previous)`,
//!   taking `Auth::proxy()` before `PUT /auth/proxy` applies;
//!   `LODESTAR_PROXY_AUTH=off` stays the recovery that needs no UI.

use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use lodestar_api::events::EventHub;
use lodestar_api::settings::{SettingSource, SettingsApi, SettingsBatch, SettingsPatch};
use lodestar_api::system::{
    AddressChange, AddressOutcome, AddressState, KeepOrRevertKind, SystemError,
};
use lodestar_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};

use super::restart::Restart;
use crate::config::Config;
use crate::ops::ui_address;
use crate::services::BoxError;
use crate::settings::network::{self, Network};
use crate::settings::{Registry, Section};

/// How long a new address waits for confirmation (§10.10.6).
pub const WINDOW_SECS: u64 = 5 * 60;

/// The environment variable that sets the address, and makes it
/// read-only in the app.
const BIND_ENV: &str = "LODESTAR_HTTP_BIND";

/// What a record guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    HttpBind,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::HttpBind => "http_bind",
        }
    }

    fn key(self) -> String {
        format!("system.keep_or_revert.{}", self.name())
    }
}

/// What a revert goes back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Previous {
    /// Nothing was saved: a revert removes the saved value.
    Reset,
    /// This value was saved.
    Saved(Value),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pending {
    AwaitingRestart,
    AwaitingConfirmation,
}

/// A pending change, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub kind: Kind,
    pub previous: Previous,
    pub new: Value,
    pub state: Pending,
    /// Unix seconds, once awaiting confirmation.
    pub deadline: Option<u64>,
}

impl Record {
    fn new_addr(&self) -> Option<SocketAddr> {
        serde_json::from_value(self.new.clone()).ok()
    }

    /// Where a revert binds: the saved value, or `base` for `reset`.
    fn previous_addr(&self, base: SocketAddr) -> SocketAddr {
        match &self.previous {
            Previous::Reset => base,
            Previous::Saved(v) => serde_json::from_value(v.clone()).unwrap_or(base),
        }
    }
}

/// Unix seconds now; tests inject their own.
pub trait Clock: Send + Sync + fmt::Debug {
    fn now(&self) -> u64;
}

#[derive(Debug)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        crate::unix_now().max(0) as u64
    }
}

fn read(store: &Store, kind: Kind) -> Option<Record> {
    let text = match store.setting_text(&kind.key()) {
        Ok(text) => text?,
        Err(e) => {
            tracing::warn!("address: reading the pending change: {e}");
            return None;
        }
    };
    match serde_json::from_str::<Record>(&text) {
        Ok(r) if r.kind == kind => Some(r),
        Ok(_) | Err(_) => {
            tracing::warn!("address: ignoring an unreadable pending change");
            None
        }
    }
}

fn write(store: &mut Store, kind: Kind, record: Option<&Record>) {
    let key = kind.key();
    let result = match record {
        Some(r) => {
            let text = serde_json::to_string(r).expect("a record serializes");
            store.update_settings_text(&[(&key, &text)], &[])
        }
        None => store.update_settings_text(&[], &[&key]),
    };
    if let Err(e) = result {
        tracing::error!("address: saving the pending change: {e}");
    }
}

/// What to do with a stored record at start.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    /// It no longer applies.
    Clear(&'static str),
    /// Its deadline passed: go back before binding.
    Revert,
    /// Try the new address, keeping a deadline already set.
    Bind {
        new: SocketAddr,
        deadline: Option<u64>,
    },
}

fn decide(record: &Record, env_set: bool, effective: SocketAddr, now: u64) -> Decision {
    if env_set {
        return Decision::Clear("LODESTAR_HTTP_BIND sets the address");
    }
    let Some(new) = record.new_addr() else {
        return Decision::Clear("the new address isn't readable");
    };
    if effective != new {
        return Decision::Clear("the address was changed outside the app");
    }
    match (record.state, record.deadline) {
        (Pending::AwaitingConfirmation, Some(d)) if now >= d => Decision::Revert,
        (Pending::AwaitingConfirmation, deadline) => Decision::Bind { new, deadline },
        (Pending::AwaitingRestart, _) => Decision::Bind {
            new,
            deadline: None,
        },
    }
}

/// What [`prepare`] did before the registry loaded.
#[derive(Debug)]
pub struct Prepared {
    /// The address without a saved value (the file's, the environment's
    /// or the default): what `reset` goes back to.
    base: SocketAddr,
    /// Bound on the pending new address.
    listener: Option<(TcpListener, Record)>,
    /// Gone back at start: (the address that didn't come up or wasn't
    /// confirmed, the one restored).
    reverted: Option<(SocketAddr, SocketAddr)>,
    /// Bind this rather than the registry's: the revert couldn't be saved.
    fallback: Option<SocketAddr>,
}

impl Prepared {
    fn nothing(config: &Config) -> Prepared {
        Prepared {
            base: config.http.bind,
            listener: None,
            reverted: None,
            fallback: None,
        }
    }
}

/// Before the registry loads: applies a stored record. A deadline already
/// past reverts; otherwise the pending address is bound now, and if that
/// fails it reverts at once. Never fails the start: anything it can't
/// read leaves the normal bind to happen.
pub async fn prepare(config: &Config, clock: &dyn Clock) -> Prepared {
    let db = config.db_path();
    if !db.is_file() {
        return Prepared::nothing(config);
    }
    let opened = {
        let (db, store_config) = (db.clone(), config.store_config());
        tokio::task::spawn_blocking(move || {
            let store = Store::open(&db, store_config).map_err(|e| e.to_string())?;
            Ok::<_, String>(read(&store, Kind::HttpBind).map(|r| (store, r)))
        })
        .await
    };
    let (store, record) = match opened {
        Ok(Ok(Some(found))) => found,
        Ok(Ok(None)) => return Prepared::nothing(config),
        Ok(Err(e)) => {
            tracing::warn!("address: opening the store to check a pending change: {e}");
            return Prepared::nothing(config);
        }
        Err(e) => {
            tracing::warn!("address: {e}");
            return Prepared::nothing(config);
        }
    };
    let env_set = config.layers.in_env(BIND_ENV);
    let effective = network::effective_bind(config);
    let now = clock.now();
    match decide(&record, env_set, effective, now) {
        Decision::Clear(why) => {
            tracing::info!("address: dropping the pending address change: {why}");
            let _ = blocking(store, move |s| write(s, Kind::HttpBind, None)).await;
            Prepared::nothing(config)
        }
        Decision::Revert => {
            let new = record.new_addr().unwrap_or(effective);
            tracing::warn!(
                "address: nobody confirmed {} within {} minutes; going back to {}",
                ui_address(new),
                WINDOW_SECS / 60,
                ui_address(record.previous_addr(config.http.bind))
            );
            revert_at_start(config, store, record, new).await
        }
        Decision::Bind { new, deadline } => match TcpListener::bind(new).await {
            Ok(listener) => {
                let record = Record {
                    state: Pending::AwaitingConfirmation,
                    deadline,
                    ..record
                };
                let stored = record.clone();
                let _ = blocking(store, move |s| write(s, Kind::HttpBind, Some(&stored))).await;
                Prepared {
                    listener: Some((listener, record)),
                    ..Prepared::nothing(config)
                }
            }
            Err(e) => {
                tracing::error!(
                    "address: binding the new address {new} failed ({e}); going back to {}",
                    ui_address(record.previous_addr(config.http.bind))
                );
                revert_at_start(config, store, record, new).await
            }
        },
    }
}

/// Runs `f` on `store` off the async runtime.
async fn blocking<T: Send + 'static>(
    mut store: Store,
    f: impl FnOnce(&mut Store) -> T + Send + 'static,
) -> Option<T> {
    tokio::task::spawn_blocking(move || f(&mut store))
        .await
        .ok()
}

/// Saves `previous` back through a registry loaded for the purpose (the
/// server's own loads after this, and sees it), then drops the record.
async fn revert_at_start(
    config: &Config,
    store: Store,
    record: Record,
    new: SocketAddr,
) -> Prepared {
    let previous = record.previous_addr(config.http.bind);
    let nothing = Prepared::nothing(config);
    let config = config.clone();
    let saved = tokio::task::spawn_blocking(move || {
        let store = Arc::new(Mutex::new(store));
        let registry = Registry::load(&config, store.clone()).map_err(|e| e.to_string())?;
        save_previous(&registry, None, &record)?;
        write(&mut store.lock().expect("store lock"), Kind::HttpBind, None);
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    match saved {
        Ok(()) => Prepared {
            reverted: Some((new, previous)),
            ..nothing
        },
        Err(e) => {
            // The record stays, so the next start tries again; this run
            // binds the previous address regardless.
            tracing::error!("address: saving the previous address failed: {e}");
            Prepared {
                reverted: Some((new, previous)),
                fallback: Some(previous),
                ..nothing
            }
        }
    }
}

/// Saves the record's previous value through `settings` and tells the
/// hub's subscribers.
fn save_previous(
    settings: &dyn SettingsApi,
    hub: Option<&EventHub>,
    record: &Record,
) -> Result<(), String> {
    let value = match &record.previous {
        Previous::Reset => Value::Null,
        Previous::Saved(v) => v.clone(),
    };
    let mut patch = Map::new();
    patch.insert(record.kind.name().to_string(), value);
    let batch = SettingsBatch(
        [(Network::SECTION.to_string(), SettingsPatch(patch))]
            .into_iter()
            .collect(),
    );
    let saved = settings.save(batch).map_err(|f| format!("{:?}", f.error))?;
    if let Some(hub) = hub {
        for (section, revision) in &saved.changed {
            hub.settings_saved(section, *revision);
        }
    }
    Ok(())
}

/// What [`bind`] bound, for [`Address::new`].
#[derive(Debug)]
pub struct Bound {
    base: SocketAddr,
    serving: SocketAddr,
    record: Option<Record>,
    reverted: Option<(SocketAddr, SocketAddr)>,
}

/// Replaces `TcpListener::bind(config.http.bind)`: the listener
/// [`prepare`] bound on the pending address (its deadline set from now
/// and stored), or the configured address.
pub async fn bind(
    prepared: Prepared,
    config: &Config,
    store: &Arc<Mutex<Store>>,
    clock: &dyn Clock,
) -> Result<(TcpListener, Bound), BoxError> {
    let Prepared {
        base,
        listener,
        reverted,
        fallback,
    } = prepared;
    let (listener, record) = match listener {
        Some((listener, record)) if record.new_addr() == Some(config.http.bind) => {
            let record = Record {
                deadline: Some(record.deadline.unwrap_or(clock.now() + WINDOW_SECS)),
                ..record
            };
            let (store, stored) = (store.clone(), record.clone());
            tokio::task::spawn_blocking(move || {
                write(
                    &mut store.lock().expect("store lock"),
                    Kind::HttpBind,
                    Some(&stored),
                );
            })
            .await?;
            tracing::warn!(
                "address: now at {}. Open it and choose \"Keep this address\" within {} \
                 minutes, or Lodestar goes back to {}",
                ui_address(config.http.bind),
                WINDOW_SECS / 60,
                ui_address(record.previous_addr(base))
            );
            (listener, Some(record))
        }
        _ => {
            let addr = fallback.unwrap_or(config.http.bind);
            let listener = TcpListener::bind(addr)
                .await
                .map_err(|e| format!("binding {addr}: {e}"))?;
            (listener, None)
        }
    };
    Restart::global().listening();
    let serving = listener.local_addr()?;
    Ok((
        listener,
        Bound {
            base,
            serving,
            record,
            reverted,
        },
    ))
}

/// The running timer: records changes, keeps, reverts.
#[derive(Debug)]
pub struct Address {
    store: Arc<Mutex<Store>>,
    settings: Arc<dyn SettingsApi>,
    hub: Option<Arc<EventHub>>,
    restart: Arc<Restart>,
    clock: Arc<dyn Clock>,
    /// What `reset` means: the file's or the default address.
    base: SocketAddr,
    env_set: bool,
    inner: Mutex<Inner>,
    /// Wakes the timer when the record changes.
    changed: Notify,
}

#[derive(Debug)]
struct Inner {
    record: Option<Record>,
    /// The registry's address and its source, as last seen.
    seen: Option<(SocketAddr, SettingSource)>,
    /// The last revert this run: (from, to).
    reverted: Option<(SocketAddr, SocketAddr)>,
    serving: SocketAddr,
}

/// Why a revert runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    Expired,
    Asked,
}

impl Address {
    pub fn new(
        bound: Bound,
        config: &Config,
        store: Arc<Mutex<Store>>,
        settings: Arc<dyn SettingsApi>,
        hub: Option<Arc<EventHub>>,
        restart: Arc<Restart>,
        clock: Arc<dyn Clock>,
    ) -> Address {
        let seen = current(settings.as_ref());
        Address {
            store,
            settings,
            hub,
            restart,
            clock,
            base: bound.base,
            env_set: config.layers.in_env(BIND_ENV),
            inner: Mutex::new(Inner {
                record: bound.record,
                seen,
                reverted: bound.reverted,
                serving: bound.serving,
            }),
            changed: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("address lock")
    }

    fn persist(&self, record: Option<&Record>) {
        write(
            &mut self.store.lock().expect("store lock"),
            Kind::HttpBind,
            record,
        );
    }

    /// `/system`'s `address_change`.
    pub fn change(&self) -> Option<AddressChange> {
        let inner = self.lock();
        if let Some(r) = &inner.record {
            let new = r.new_addr()?;
            return Some(AddressChange {
                deadline: match r.state {
                    Pending::AwaitingConfirmation => r.deadline,
                    Pending::AwaitingRestart => None,
                },
                kind: KeepOrRevertKind::HttpBind,
                previous_url: ui_address(r.previous_addr(self.base)),
                state: match r.state {
                    Pending::AwaitingRestart => AddressState::AwaitingRestart,
                    Pending::AwaitingConfirmation => AddressState::AwaitingConfirmation,
                },
                url: ui_address(new),
            });
        }
        inner.reverted.map(|(from, to)| AddressChange {
            deadline: None,
            kind: KeepOrRevertKind::HttpBind,
            previous_url: ui_address(to),
            state: AddressState::Reverted,
            url: ui_address(from),
        })
    }

    /// Follows the registry's `network` section, recording each change
    /// of the address, and runs the deadline. Both stop with the runtime.
    pub fn start(self: &Arc<Self>, mut network: watch::Receiver<Network>) {
        let me = self.clone();
        tokio::spawn(async move {
            while network.changed().await.is_ok() {
                let me = me.clone();
                if tokio::task::spawn_blocking(move || me.saved())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let me = self.clone();
        tokio::spawn(async move { me.run_timer().await });
    }

    /// A save changed the `network` section: records an address change.
    fn saved(&self) {
        if self.env_set {
            return;
        }
        let Some((addr, source)) = current(self.settings.as_ref()) else {
            return;
        };
        let mut inner = self.lock();
        let before = inner.seen.replace((addr, source));
        let Some((before, before_source)) = before else {
            return;
        };
        if addr == before {
            return;
        }
        let record = match inner.record.take() {
            // Back where it was: nothing is pending any more.
            Some(r) if r.previous_addr(self.base) == addr => None,
            // Another change: the original `previous` is the one that worked.
            Some(r) => Some(Record {
                new: json(addr),
                state: Pending::AwaitingRestart,
                deadline: None,
                ..r
            }),
            None => Some(Record {
                kind: Kind::HttpBind,
                previous: match before_source {
                    SettingSource::Db => Previous::Saved(json(before)),
                    _ => Previous::Reset,
                },
                new: json(addr),
                state: Pending::AwaitingRestart,
                deadline: None,
            }),
        };
        self.persist(record.as_ref());
        inner.record = record;
        drop(inner);
        self.changed.notify_waiters();
    }

    /// `POST /system/address/keep`: confirms the new address.
    pub fn keep(&self) -> Result<AddressOutcome, SystemError> {
        let mut inner = self.lock();
        match &inner.record {
            Some(r) if r.state == Pending::AwaitingConfirmation => {
                tracing::info!("address: kept {}", ui_address(inner.serving));
                self.persist(None);
                inner.record = None;
                drop(inner);
                self.changed.notify_waiters();
                Ok(AddressOutcome {
                    address_change: None,
                    restarting: false,
                })
            }
            _ => Err(SystemError::NoAddressChange),
        }
    }

    /// `POST /system/address/revert`. The caller asks for the restart
    /// once its answer is out.
    pub fn revert(&self) -> Result<AddressOutcome, SystemError> {
        self.revert_for(Why::Asked)
    }

    fn revert_for(&self, why: Why) -> Result<AddressOutcome, SystemError> {
        let mut inner = self.lock();
        let Some(record) = inner.record.clone() else {
            return Err(SystemError::NoAddressChange);
        };
        let now = self.clock.now();
        if why == Why::Expired
            && (record.state != Pending::AwaitingConfirmation
                || record.deadline.is_none_or(|d| now < d))
        {
            return Err(SystemError::NoAddressChange);
        }
        let previous = record.previous_addr(self.base);
        save_previous(self.settings.as_ref(), self.hub.as_deref(), &record)
            .map_err(SystemError::Internal)?;
        // Seen before the follower wakes, so it doesn't record the revert
        // as a change of its own.
        inner.seen = current(self.settings.as_ref());
        inner.record = None;
        self.persist(None);
        let restarting = match record.state {
            // Never applied: undoing the save is enough.
            Pending::AwaitingRestart => {
                tracing::info!(
                    "address: the change to {} was undone before a restart",
                    record.new_addr().map_or_else(String::new, ui_address)
                );
                false
            }
            Pending::AwaitingConfirmation => {
                inner.reverted = Some((inner.serving, previous));
                match why {
                    Why::Expired => tracing::warn!(
                        "address: nobody confirmed {} within {} minutes; going back to {}",
                        ui_address(inner.serving),
                        WINDOW_SECS / 60,
                        ui_address(previous)
                    ),
                    Why::Asked => {
                        tracing::info!("address: going back to {} as asked", ui_address(previous))
                    }
                }
                if !self.restart.available() {
                    tracing::warn!(
                        "address: {} applies when Lodestar is next restarted",
                        ui_address(previous)
                    );
                }
                self.restart.available()
            }
        };
        drop(inner);
        self.changed.notify_waiters();
        Ok(AddressOutcome {
            address_change: self.change(),
            restarting,
        })
    }

    /// Reverts if the deadline has passed, then restarts onto the previous
    /// address where this process can. True if it reverted.
    fn expire(&self) -> bool {
        match self.revert_for(Why::Expired) {
            Ok(outcome) => {
                if outcome.restarting {
                    self.restart.request();
                }
                true
            }
            Err(SystemError::NoAddressChange) => false,
            Err(e) => {
                // The deadline has passed and the record stays: a restart
                // goes back before binding, through a fresh registry.
                tracing::error!("address: going back failed: {e:?}");
                if self.restart.available() {
                    self.restart.request();
                }
                false
            }
        }
    }

    fn deadline(&self) -> Option<u64> {
        let inner = self.lock();
        inner
            .record
            .as_ref()
            .filter(|r| r.state == Pending::AwaitingConfirmation)
            .and_then(|r| r.deadline)
    }

    async fn run_timer(self: Arc<Self>) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.deadline() {
                None => notified.await,
                Some(deadline) => {
                    let now = self.clock.now();
                    if now >= deadline {
                        let me = self.clone();
                        let reverted = tokio::task::spawn_blocking(move || me.expire())
                            .await
                            .unwrap_or(false);
                        if !reverted {
                            // A failed save: try again in a minute.
                            tokio::time::sleep(Duration::from_secs(60)).await;
                        }
                        continue;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(deadline - now)) => {}
                        _ = notified => {}
                    }
                }
            }
        }
    }
}

fn json(addr: SocketAddr) -> Value {
    Value::String(addr.to_string())
}

/// The registry's address now, and where it comes from.
fn current(settings: &dyn SettingsApi) -> Option<(SocketAddr, SettingSource)> {
    let values = settings.section(Network::SECTION)?;
    let field = values.fields.get(Kind::HttpBind.name())?;
    let addr = serde_json::from_value(field.value.clone()).ok()?;
    Some((addr, field.source))
}

#[cfg(test)]
mod tests;
