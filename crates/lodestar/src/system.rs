//! `/system` for the running server (architecture §10.10.6, §10.10.9,
//! §10.10.13): [`System`] implements `lodestar_api::system::SystemApi`
//! over the registry, the pipeline, the index and the data directory.
//!
//! - [`info`]: the build, the host, the deployment and the data
//!   directory's sizes (cached for a minute).
//! - [`restart`]: the request flag `main.rs` races with the stop signals,
//!   and the re-exec.
//! - [`address`]: the HTTP address's keep-or-revert timer.

pub mod address;
pub mod info;
pub mod restart;

use std::sync::{Arc, Mutex};

use lodestar_api::dto::events::{RebuildEvent, RebuildPhase};
use lodestar_api::ops::{BoxFuture, OpsApi};
use lodestar_api::settings::{SettingSource, SettingsApi};
use lodestar_api::system::{
    AddressOutcome, ChangedSetting, Connection, ConnectionKind, ConnectionState, DataFormats,
    DataInfo, Diagnostics, RestartInfo, SystemApi, SystemError, SystemInfo,
};
use lodestar_search::{IndexVersion, RebuildProgress, SearchHandle};
use lodestar_store::Store;

use lodestar_api::events::EventHub;
use lodestar_companions::manager::Snapshot;
use tokio::sync::watch;

use crate::config::Config;
use crate::settings::Registry;
use crate::settings::crawler::Crawler;

pub use address::Address;
pub use restart::Restart;

/// The running server, as `/system` reports it.
pub struct System {
    pub settings: Arc<dyn SettingsApi>,
    /// The pipeline's sources; `None` when none runs (tests).
    pub ops: Option<Arc<dyn OpsApi>>,
    /// The registry's crawler section: whether the crawler is paused.
    pub crawler: watch::Receiver<Crawler>,
    pub data_dir: std::path::PathBuf,
    /// A read-only connection: the schema version and the subscriptions.
    pub reader: Arc<Mutex<Store>>,
    pub search: Option<SearchHandle>,
    pub address: Arc<Address>,
    pub restart: Arc<Restart>,
    pub sizes: Arc<info::Sizes>,
    /// Whether the DHT runs in this process.
    pub dht: bool,
    /// The update engine's row: whether updates are on and the last
    /// completed check; `None` without an engine.
    pub updates: Option<UpdatesRow>,
    /// The companion manager's snapshot (CMP-6's row); `None` when
    /// companions are off for this run.
    pub companions: Option<watch::Receiver<Snapshot>>,
}

/// What `/system`'s rows read from other services.
pub struct Rows {
    pub updates: Option<UpdatesRow>,
    pub companions: Option<watch::Receiver<Snapshot>>,
}

/// What `/system`'s Updates row reads from the update engine.
pub type UpdatesRow = Arc<dyn Fn() -> (bool, Option<i64>) + Send + Sync>;

impl std::fmt::Debug for System {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("System").finish_non_exhaustive()
    }
}

/// What the reader gives: the store's schema version and the enabled
/// subscriptions with their last check.
fn from_store(store: &Store) -> (Option<i64>, Option<(u64, Option<u64>)>) {
    let schema = store
        .connection()
        .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .ok()
        .flatten();
    let subscriptions = store.subscriptions().ok().map(|subs| {
        let enabled: Vec<_> = subs.iter().filter(|s| s.enabled).collect();
        let last = enabled
            .iter()
            .filter_map(|s| s.checked_at)
            .max()
            .map(|t| t.max(0) as u64);
        (enabled.len() as u64, last)
    });
    (schema, subscriptions)
}

/// Saved settings waiting for a restart, and every field not at its
/// default with its source: names only, never values.
fn settings_state(settings: &dyn SettingsApi) -> (Vec<String>, Vec<ChangedSetting>) {
    let mut restart = Vec::new();
    let mut changed = Vec::new();
    for section in settings.index().sections {
        let Some(values) = settings.section(&section.section) else {
            continue;
        };
        restart.extend(
            values
                .restart_required
                .iter()
                .map(|f| format!("{}.{f}", values.section)),
        );
        for (name, field) in &values.fields {
            if field.source != SettingSource::Default {
                changed.push(ChangedSetting {
                    name: format!("{}.{name}", values.section),
                    source: field.source,
                });
            }
        }
    }
    (restart, changed)
}

fn on_off(on: bool) -> ConnectionState {
    if on {
        ConnectionState::On
    } else {
        ConnectionState::Off
    }
}

/// The Companions row: how many companions Lodestar installed, `on` when
/// any is. `off` with no count when companions are off for this run.
fn companions_row(snapshot: Option<&watch::Receiver<Snapshot>>) -> Connection {
    let installed = snapshot.map(|rx| {
        rx.borrow()
            .companions
            .iter()
            .filter(|c| c.version.is_some())
            .count() as u64
    });
    Connection {
        count: installed,
        kind: ConnectionKind::Companions,
        last_checked_at: None,
        settings: "companions".to_string(),
        state: Some(on_off(installed.is_some_and(|n| n > 0))),
    }
}

impl System {
    fn connections(
        &self,
        sources: Option<u64>,
        subscriptions: Option<(u64, Option<u64>)>,
    ) -> Vec<Connection> {
        let row = |kind, settings: &str, state, count, last_checked_at| Connection {
            count,
            kind,
            last_checked_at,
            settings: settings.to_string(),
            state,
        };
        let paused = self.crawler.borrow().paused;
        let dht = if !self.dht {
            ConnectionState::Off
        } else if paused {
            ConnectionState::Paused
        } else {
            ConnectionState::On
        };
        vec![
            row(ConnectionKind::Dht, "network", Some(dht), None, None),
            row(
                ConnectionKind::Sources,
                "sources",
                sources.map(|n| on_off(n > 0)),
                sources,
                None,
            ),
            row(
                ConnectionKind::BlocklistSubscriptions,
                "filters",
                subscriptions.map(|(n, _)| on_off(n > 0)),
                subscriptions.map(|(n, _)| n),
                subscriptions.and_then(|(_, at)| at),
            ),
            // Download clients and notification channels aren't stored
            // yet; workers come in v0.6.
            row(
                ConnectionKind::Integrations,
                "integrations",
                None,
                None,
                None,
            ),
            {
                let updates = self.updates.as_ref().map(|f| f());
                row(
                    ConnectionKind::Updates,
                    "updates",
                    updates.map(|(on, _)| on_off(on)),
                    None,
                    updates.and_then(|(_, at)| at).map(|t| t.max(0) as u64),
                )
            },
            companions_row(self.companions.as_ref()),
            row(ConnectionKind::Workers, "workers", None, None, None),
        ]
    }
}

impl System {
    /// Builds `/system` for `Services::start`, once the listener is bound
    /// and the hub exists, and starts the address timer.
    pub fn start(
        config: &Config,
        bound: address::Bound,
        api: &lodestar_api::ApiState,
        settings: &Arc<Registry>,
        control: Arc<Mutex<Store>>,
        events: &Arc<EventHub>,
        rows: Rows,
    ) -> Arc<dyn SystemApi> {
        let Rows {
            updates,
            companions,
        } = rows;
        let restart = Restart::global().clone();
        let address = Arc::new(Address::new(
            bound,
            config,
            control,
            settings.clone(),
            Some(events.clone()),
            restart.clone(),
            Arc::new(address::SystemClock),
        ));
        address.start(settings.subscribe());
        Arc::new(System {
            settings: settings.clone(),
            ops: api.ops.clone(),
            crawler: settings.subscribe(),
            data_dir: config.data_dir.clone(),
            reader: api.reader.clone(),
            search: api.index.clone(),
            address,
            restart,
            sizes: Arc::new(info::Sizes::new(config.data_dir.clone())),
            dht: config.dht.enabled,
            updates,
            companions,
        })
    }
}

impl SystemApi for System {
    fn info(&self) -> BoxFuture<'_, SystemInfo> {
        Box::pin(async move {
            let (sizes, reader, settings) = (
                self.sizes.clone(),
                self.reader.clone(),
                self.settings.clone(),
            );
            let blocking = tokio::task::spawn_blocking(move || {
                let data = sizes.get();
                let (schema, subscriptions) = from_store(&reader.lock().expect("reader lock"));
                let (restart_required, changed) = settings_state(settings.as_ref());
                (data, schema, subscriptions, restart_required, changed)
            })
            .await;
            let (data, schema, subscriptions, restart_required, changed) = match blocking {
                Ok(parts) => parts,
                // Only a panic gets here: unknown, rather than no answer.
                Err(_) => (
                    DataInfo {
                        cold_bytes: None,
                        database_bytes: None,
                        dir: self.data_dir.display().to_string(),
                        filesystem_warning: None,
                        free_bytes: None,
                        index_bytes: None,
                        logs_bytes: None,
                    },
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                ),
            };
            // Every source but the DHT, which has its own row.
            let sources = match &self.ops {
                Some(ops) => ops.sources().await.ok(),
                None => None,
            };
            let sources = sources.map(|all| {
                all.iter()
                    .filter(|s| s.plugin != "dht" && s.enabled)
                    .count() as u64
            });
            // Off the runtime: a revert holds the address lock across a save.
            let address = self.address.clone();
            let address_change = tokio::task::spawn_blocking(move || address.change())
                .await
                .ok()
                .flatten();
            let started_at = self.restart.started_at();
            SystemInfo {
                address_change,
                build: info::build(),
                channel: info::channel(),
                connections: self.connections(sources, subscriptions),
                data,
                data_formats: DataFormats {
                    index: IndexVersion::CURRENT.dir_name(),
                    rebuild: self
                        .search
                        .as_ref()
                        .and_then(SearchHandle::progress)
                        .map(|p| rebuild_event(&p)),
                    store_schema: schema,
                },
                deployment: info::deployment_here(),
                diagnostics: Diagnostics {
                    changed,
                    logging: None,
                    recent_errors: None,
                },
                fixture: crate::fixture::active(),
                host: info::host(),
                repository: env!("CARGO_PKG_REPOSITORY").to_string(),
                restart: self.restart.info(),
                restart_required,
                started_at,
                uptime_s: (crate::unix_now().max(0) as u64).saturating_sub(started_at),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }
        })
    }

    fn restart_info(&self) -> RestartInfo {
        self.restart.info()
    }

    fn prepare_restart(&self) -> Result<(), SystemError> {
        // Logged as `app.restart_failed` with the error kind; the answer
        // carries no detail.
        self.restart
            .prepare()
            .map_err(|_| SystemError::RestartFailed)
    }

    fn restart(&self) {
        self.restart.request();
    }

    fn keep_address(&self) -> BoxFuture<'_, Result<AddressOutcome, SystemError>> {
        let address = self.address.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || address.keep())
                .await
                .map_err(|e| SystemError::Internal(e.to_string()))?
        })
    }

    fn revert_address(&self) -> BoxFuture<'_, Result<AddressOutcome, SystemError>> {
        let address = self.address.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || address.revert())
                .await
                .map_err(|e| SystemError::Internal(e.to_string()))?
        })
    }
}

/// The rebuild's progress as the `rebuild` event carries it.
fn rebuild_event(p: &RebuildProgress) -> RebuildEvent {
    use lodestar_search::RebuildPhase as P;
    RebuildEvent {
        done: p.done,
        error: p.error.clone(),
        from: p.from.to_string(),
        phase: match p.phase {
            P::Building => RebuildPhase::Building,
            P::Finishing => RebuildPhase::Finishing,
            P::Swapping => RebuildPhase::Swapping,
            P::Failed => RebuildPhase::Failed,
        },
        to: p.to.to_string(),
        total: p.total,
        unavailable: p.unavailable.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_companions::manager::{Health, Present};
    use lodestar_companions::{CompanionId, Version};

    #[test]
    fn the_companions_row_counts_what_lodestar_installed() {
        let off = companions_row(None);
        assert_eq!(
            (off.kind, off.state, off.count, off.settings.as_str()),
            (
                ConnectionKind::Companions,
                Some(ConnectionState::Off),
                None,
                "companions"
            )
        );
        let (tx, rx) = watch::channel(Snapshot::default());
        let none = companions_row(Some(&rx));
        assert_eq!(
            (none.state, none.count),
            (Some(ConnectionState::Off), Some(0))
        );
        let present = |id: &str, v: Option<&str>| Present {
            id: CompanionId::parse(id).unwrap(),
            version: v.map(|v| Version::parse(v).unwrap()),
            health: Health::Unknown,
        };
        tx.send_modify(|s| {
            s.companions = vec![
                present("qbittorrent", Some("5.1.2")),
                present("prowlarr", None),
            ]
        });
        let one = companions_row(Some(&rx));
        assert_eq!((one.state, one.count), (Some(ConnectionState::On), Some(1)));
        assert_eq!(one.last_checked_at, None);
    }
}
