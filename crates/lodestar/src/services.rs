//! Builds and wires every long-running service of `lodestar serve`: the
//! store and its readers, the blocklist subscriptions, the pipeline, the
//! live indexer, the filters API, the settings registry, the API state
//! and the HTTP server. `main.rs` parses the command line and calls
//! [`Services::start`]; [`Services::run`] serves until a signal, then
//! shuts down in order.
//!
//! A new service is built in [`Services::start`], keeps its handle in
//! [`Handles`], and stops in [`Handles::shutdown`].

use std::future::Future;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Extension;
use lodestar_api::dto::events::CrawlerEvent;
use lodestar_api::events::{EventHub, EventSources, EventsConfig};
use lodestar_filter::Sealer;
use lodestar_filter::lists::SharedIps;
use lodestar_pipeline::gate::GateStats;
use lodestar_pipeline::health::HealthHandle;
use lodestar_pipeline::{Control, DhtSource, HealthSetup, Pipeline, Rollups, metric};
use lodestar_search::{CommitHook, LiveConfig, Ranking, SearchHandle, Serving, ServingConfig};
use lodestar_sources::SourcePlugin;
use lodestar_store::{HotColumns, Store, hot};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::{floor, http, insights, ops, settings, subscriptions, system};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Everything `serve` runs, started and wired.
pub struct Services {
    /// The API's state (auth, index, reader), as `/api/v1` sees it.
    pub api: lodestar_api::ApiState,
    /// Export's own adult setting, followed live (§20.9).
    pub export_adult: lodestar_api::export::ExportAdult,
    /// The status page's and metrics' state.
    pub http: http::AppState,
    /// `/api/v1/events`: the live stream's hub.
    pub events: Arc<EventHub>,
    pub handles: Handles,
    listener: TcpListener,
    /// `/system` (B2c), layered beside the hub.
    system: Arc<dyn lodestar_api::system::SystemApi>,
    /// `/updates` (B-update), beside `/system`.
    updates: Arc<dyn lodestar_api::updates::UpdatesApi>,
}

/// What has to be stopped on the way out.
pub struct Handles {
    /// The blocklist refresher (§20.13) and its stop signal.
    subscriptions: (watch::Sender<bool>, JoinHandle<()>),
    pipeline: Pipeline,
    /// The index, its live indexer and any rebuild (§8).
    search: Option<Arc<Serving>>,
    /// Swarm stats (§7), snapshotted to `hot_dir` after the pipeline stops.
    hot: Arc<HotColumns>,
    hot_dir: std::path::PathBuf,
    /// Insights' searchable-time recorder and its stop signal.
    insights: (watch::Sender<bool>, JoinHandle<()>),
    /// Saved searches → alerts (B9), while search is on.
    percolator: Option<(watch::Sender<bool>, JoinHandle<()>)>,
    /// The update engine's scheduled checks (B-update).
    updates: Option<(watch::Sender<bool>, JoinHandle<()>)>,
    /// The storage size sampler (R-gauges).
    storage: (watch::Sender<bool>, JoinHandle<()>),
}

impl Services {
    /// Opens the data directory and starts every service. The HTTP
    /// listener is bound but serves nothing until [`Services::run`].
    pub async fn start(config: &Config) -> Result<Services, BoxError> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|e| format!("creating {}: {e}", config.data_dir.display()))?;
        // `/system`'s start time, whichever entry point runs this (B2c).
        system::Restart::global().started();
        // A pending HTTP-address change, before the registry loads (B2c).
        let address = system::address::prepare(config, &system::address::SystemClock).await;
        // The settings registry (§11) over its own connection: saved
        // values join the file and environment before anything starts.
        let settings = {
            let (db, store_config, base) =
                (config.db_path(), config.store_config(), config.clone());
            tokio::task::spawn_blocking(move || {
                let store = Arc::new(Mutex::new(Store::open(&db, store_config)?));
                Ok::<_, BoxError>(Arc::new(settings::Registry::load(&base, store)?))
            })
            .await??
        };
        let config = &settings.effective_config(config);
        // The crawler's pause (B14) outlives a restart: everything starts
        // paused, so nothing is harvested or fetched before it applies.
        let crawl_settings = settings
            .subscribe::<settings::crawler::Crawler>()
            .borrow()
            .clone();
        let mut pipeline_config = config.pipeline_config()?;
        pipeline_config.pool.paused = crawl_settings.paused;
        let key = seal_key(&config.data_dir.join("seal.key"))?;
        pipeline_config.sealer = Some(Arc::new(Sealer::new(&key)));
        // A fixture is served offline: no tracker is ever asked (its swarm
        // rows come from the manifest).
        let health = config
            .health_config()?
            .filter(|_| !crate::fixture::active());
        // Subscribed IP lists (§20.13): one shared set, consulted by the
        // fetch pool, the DHT and the tracker scraper before talking to
        // anyone.
        let ips = Arc::new(SharedIps::default());
        let denied = {
            let ips = ips.clone();
            Arc::new(move |ip: IpAddr| ips.contains(ip))
        };
        pipeline_config.fetch.deny = Some(lodestar_peer_wire::Deny(denied.clone()));
        // Fetch attempts and successes per transport, for /metrics.
        let fetch_stats = pipeline_config.fetch.stats.clone();
        let db = config.db_path();
        let store_config = config.store_config();
        let search = config.search.enabled;
        // The root holds one directory per index version (§8); this
        // build's is the one `index_path` names.
        let index_path = config.index_path();
        let index_root = index_path.parent().unwrap_or(&index_path).to_path_buf();
        // The storage sampler walks the same root (R-gauges).
        let storage_index = index_root.clone();
        let heap = config.search.heap_mib.max(16) << 20;
        let filter = pipeline_config.filter.clone();
        let subs_config = config.subscriptions.clone();
        let blocklist = config.blocklist_files()?;
        let subs_ips = ips.clone();
        let hot_dir = config.data_dir.join("hot");
        let weights = config.ranking.weights();
        let suspect_penalty = config.ranking.suspect.clamp(0.0, 1.0);
        // The scrape scheduler opens its own connection (below).
        let (db2, store_config2, hot_dir2) = (db.clone(), store_config.clone(), hot_dir.clone());
        let (store, control, user_data, reader, serving, percolation, hot) =
            tokio::task::spawn_blocking(move || {
                let (db, store_config) = (db2, store_config2);
                let mut store = Store::open(&db, store_config.clone())?;
                // After an unclean stop, the counts a crash can lose are
                // recounted (§7), before the pipeline writes any.
                match insights::repair_after_unclean_stop(&mut store) {
                    Ok(Some(r)) if r.changed() => {
                        lodestar_log::log_event!(crate::logging::events::INSIGHTS_REPAIRED)
                    }
                    Ok(_) => {}
                    Err(e) => lodestar_log::log_event!(
                        crate::logging::events::INSIGHTS_STEP_FAILED,
                        step = "repair",
                        error = lodestar_log::Error::new(&e)
                    ),
                }
                // Settings and lists change through a second connection, so
                // they never wait behind the ingest writer for long.
                let mut control = Store::open(&db, store_config.clone())?;
                // The pipeline loads the rest of the filter at its start
                // (`filter_load`, shared with `lodestar import`).
                crate::filter_load::sync_sources(
                    &mut control,
                    &subs_config,
                    &blocklist,
                    &filter,
                    &subs_ips,
                    crate::unix_now(),
                )?;
                floor::report(&control)?;
                // Swarm stats for search and the writer (§7): last run's
                // snapshot, or rebuilt from SQLite after a crash.
                let started = std::time::Instant::now();
                let (hot, from_snapshot) = lodestar_pipeline::open_hot(&mut store, &hot_dir2)?;
                lodestar_log::log_event!(
                    crate::logging::events::APP_HOT_COLUMNS,
                    torrents = hot.len(),
                    ms = started.elapsed().as_millis() as u64,
                    from = if from_snapshot { "snapshot" } else { "store" }
                );
                let hot = Arc::new(hot);
                let user_data = Store::open(&db, store_config.clone())?;
                let reader = Store::open_reader(&db, store_config.clone())?;
                // The percolator records alerts through its own connection.
                let percolation = search
                    .then(|| Store::open(&db, store_config.clone()))
                    .transpose()?;
                // The index this build serves (rebuilt in the background from
                // an older one if need be); its indexer follows the store
                // through its own connection. Every index it serves ranks
                // with the swarm stats.
                let serving = if search {
                    let suspect = hot.clone();
                    let ranking = Ranking {
                        hot: Some(hot.clone()),
                        weights,
                        // §8: a fake-seeding swarm ranks well below its claim.
                        penalty: Some(Arc::new(move |id| {
                            if suspect.flags(id) & hot::flags::SUSPECT != 0 {
                                suspect_penalty
                            } else {
                                1.0
                            }
                        })),
                    };
                    let config = ServingConfig {
                        live: LiveConfig {
                            heap_bytes: heap,
                            ..LiveConfig::default()
                        },
                        store: store_config,
                        ranking,
                        ..ServingConfig::default()
                    };
                    Some(Arc::new(Serving::open_or_rebuild(
                        &index_root,
                        &db,
                        config,
                    )?))
                } else {
                    None
                };
                Ok::<_, BoxError>((store, control, user_data, reader, serving, percolation, hot))
            })
            .await??;

        let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
        let mut dht_stats = None;
        let mut dht_node = None;
        let mut dht_source = None;
        if config.dht.enabled {
            let mut dht_config = config.dht_config();
            dht_config.deny = Some(lodestar_dht::node::Deny(denied.clone()));
            dht_config.paused = crawl_settings.paused;
            let dht = Arc::new(DhtSource::new(dht_config));
            dht_stats = Some(dht.stats_handle());
            dht_node = Some(dht.node_handle());
            settings::crawler::follow_intensity(settings.subscribe(), dht.node_handle());
            dht_source = Some(dht.clone());
            plugins.push(dht);
        }
        pipeline_config.hot = Some(hot.clone());
        // The gate's searchable count follows the serving index (below).
        pipeline_config.gate_follows_index = serving.is_some();
        pipeline_config.health = health.map(|(cfg, trackers)| HealthSetup {
            config: lodestar_pipeline::health::HealthConfig {
                deny: Some(lodestar_tracker_scrape::Deny(denied.clone())),
                ..cfg
            },
            trackers,
            db: db.clone(),
            store: store_config.clone(),
            dht: dht_node.clone(),
        });
        let dht_ops_node = dht_node;
        let auth = Arc::new(
            lodestar_api::Auth::load(
                &config.data_dir.join("auth.json"),
                config.http.secure_cookies,
            )
            .map_err(|e| format!("loading auth: {e}"))?,
        );
        // The way back in after a proxy change locks you out (§12).
        let proxy_env = std::env::var(lodestar_api::auth::PROXY_AUTH_ENV).ok();
        if auth.apply_proxy_override(proxy_env.as_deref()) {
            lodestar_log::log_event!(lodestar_api::auth::log_events::AUTH_PROXY_FORCED_OFF);
        }
        if let Some(token) = auth.setup_token() {
            // §12: whoever can read the logs claims the instance. The
            // console shows the token right after the message.
            lodestar_log::log_event!(
                crate::logging::events::AUTH_SETUP_TOKEN,
                token = lodestar_log::SetupToken(&token),
                address = lodestar_log::Listen(config.http.bind)
            );
        }
        let filter = pipeline_config.filter.clone();
        let held = pipeline_config.held.clone();
        let sealer = pipeline_config.sealer.clone();
        let reenrich = pipeline_config.reenrich.clone();
        let quarantine_secs = pipeline_config.quarantine.as_secs() as i64;
        let pipeline = Pipeline::start(pipeline_config, store, plugins).await?;
        let control = Arc::new(Mutex::new(control));
        // Pause, resume, sources and the network check (B14), applied
        // however the registry's crawler section changes.
        let crawl = Control::start(
            pipeline.monitor(),
            dht_source,
            control.clone(),
            crawl_settings.paused,
            crawl_settings.intensity.into(),
        )
        .await?;
        settings::crawler::follow_control(settings.subscribe(), crawl.clone());
        let search = serving.clone().map(SearchHandle::serving);
        let filters = Arc::new(lodestar_api::filters::FiltersApi {
            store: control.clone(),
            engine: filter.clone(),
            held,
            sealer,
            ips: ips.clone(),
            index: search.clone(),
            reenrich,
            jobs: Default::default(),
            quarantine_secs,
        });
        let (stop_subscriptions, subscriptions_stopped) = watch::channel(false);
        let subscriptions_stopped = crate::fixture::unless_offline(subscriptions_stopped);
        let refresher = tokio::spawn(subscriptions::run(
            control.clone(),
            subscriptions::Limits::from_config(&config.subscriptions),
            filter,
            ips,
            subscriptions_stopped,
        ));
        let reader = Arc::new(Mutex::new(reader));
        let search_state = serving.as_ref().map(|s| http::SearchState {
            index: SearchHandle::serving(s.clone()),
            stats: s.stats(),
        });
        let paused_now = crawl.clone();
        if let Some(s) = &serving {
            follow_gate(pipeline.monitor().gate().clone(), s.commit_hook());
        }
        // Insights (B11a): searchable time from the index's commits.
        let insights = Arc::new(insights::Insights::new(pipeline.monitor()));
        // What the stage pages read beside `OpsApi` (B11b).
        if let Some(dht) = dht_stats.clone() {
            insights.set_snapshot(Box::new(move || lodestar_api::insights::StageSnapshot {
                dht_reply_ratio: (*dht.lock().unwrap_or_else(|e| e.into_inner()))
                    .filter(|d| d.queries_out > 0)
                    .map(|d| d.reply_ratio()),
            }));
        }
        let (stop_insights, insights_stopped) = watch::channel(false);
        let recorder = {
            let crawl = crawl.clone();
            insights::spawn(
                insights.clone(),
                insights::Follow {
                    db: config.db_path(),
                    store: config.store_config(),
                    index: serving
                        .as_ref()
                        .map(|s| (s.commit_hook().clone(), s.stats())),
                    paused: Box::new(move || crawl.status().paused),
                    // As `/stats` counts them.
                    searchable: {
                        let index = search.clone();
                        Box::new(move || index.as_ref().map(|i| i.index().num_docs()))
                    },
                    tick: insights::TICK,
                },
                insights_stopped,
            )
        };
        let ops = Arc::new(
            ops::Ops::new(
                crawl,
                settings.subscribe(),
                pipeline.monitor(),
                search_state.clone(),
                config.data_dir.clone(),
            )
            .with_dht(
                dht_stats
                    .clone()
                    .zip(dht_ops_node)
                    .map(|(stats, node)| ops::DhtReport { stats, node }),
            ),
        );
        let monitor = pipeline.monitor();
        let api = lodestar_api::ApiState {
            auth,
            index: search.clone(),
            reader: reader.clone(),
            filters: Some(filters),
            settings: Some(settings.clone()),
            hot: Some(hot.clone()),
            events: Some(Arc::new(Events {
                health: monitor.health().cloned(),
                rollups: monitor.rollups().clone(),
            })),
            ops: Some(ops),
            user_data: Some(Arc::new(Mutex::new(user_data))),
            adult: settings::content::follow(settings.subscribe()),
            insights: Some(insights),
        };
        settings::log::follow(settings.subscribe());
        let storage = Arc::new(crate::storage::Gauges::default());
        let (stop_storage, storage_stopped) = watch::channel(false);
        let sampler = crate::storage::spawn(
            crate::storage::Paths {
                db: config.db_path(),
                index: storage_index,
            },
            storage.clone(),
            crate::storage::INTERVAL,
            storage_stopped,
        );
        let http = http::AppState {
            monitor: pipeline.monitor(),
            dht: dht_stats,
            fetch: fetch_stats,
            reader,
            search: search_state,
            storage: Some(storage),
        };
        // The live stream (§9): new torrents from the serving index's
        // commits, crawler snapshots from the status page's counters, and
        // rebuild progress.
        let crawler = {
            let (monitor, dht) = (http.monitor.clone(), http.dht.clone());
            let control = paused_now;
            move || {
                let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
                let p = monitor.pool_stats();
                CrawlerEvent {
                    at: crate::unix_now().max(0) as u64,
                    dht_nodes: dht
                        .as_ref()
                        .and_then(|d| *d.lock().unwrap_or_else(|e| e.into_inner()))
                        .map(|d| d.routing_nodes),
                    fetch_active: get(&p.active),
                    fetch_pending: get(&p.pending),
                    fetched: get(&p.fetched),
                    harvested: get(&monitor.intake_stats().received),
                    paused: control.status().paused,
                    stored: get(&monitor.writer_stats().stored),
                    torrents: monitor.torrents(),
                }
            }
        };
        let events = EventHub::start(
            EventsConfig::default(),
            EventSources {
                commits: serving.as_ref().map(|s| s.commit_hook().clone()),
                crawler: Some(Arc::new(crawler)),
                rebuild: search.clone(),
                adult: api.adult.clone(),
                insights: Some(api.clone()),
            },
        );
        // Saved searches → alerts (B9): it follows the serving index's
        // commits and publishes on the hub. Never in fixture mode.
        let percolator = serving.as_ref().zip(percolation).map(|(s, store)| {
            let (stop, stopped) = watch::channel(false);
            let task = crate::percolator::spawn(
                SearchHandle::serving(s.clone()),
                s.commit_hook(),
                store,
                events.clone(),
                api.adult.clone(),
                crate::fixture::unless_offline(stopped),
                crate::percolator::PercolatorConfig::default(),
            );
            (stop, task)
        });
        let (listener, bound) =
            system::address::bind(address, config, &control, &system::address::SystemClock).await?;
        let served = bound.serving;
        // The update engine (B-update): none for a fixture.
        let updates = crate::update::start(config, &settings, &events);
        let system = system::System::start(
            config,
            bound,
            &api,
            &settings,
            control,
            &events,
            updates.row.clone(),
        );
        lodestar_log::log_event!(
            crate::logging::events::APP_SERVING,
            address = lodestar_log::Listen(served),
            path = lodestar_log::Path::new(&config.data_dir)
        );
        Ok(Services {
            export_adult: settings::content::follow_export(settings.subscribe()),
            api,
            http,
            events,
            handles: Handles {
                subscriptions: (stop_subscriptions, refresher),
                pipeline,
                search: serving,
                hot,
                hot_dir,
                insights: (stop_insights, recorder),
                percolator,
                updates: updates.task,
                storage: (stop_storage, sampler),
            },
            listener,
            system,
            updates: updates.api,
        })
    }

    /// What an update's self-test probes (§12.2, B-update): the bound HTTP
    /// address, whether the DHT is up (when it's on), and the index.
    #[cfg(unix)]
    pub fn probe(&self) -> crate::update::probation::Probe {
        crate::update::probation::Probe {
            http: self
                .listener
                .local_addr()
                .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0))),
            dht: self.http.dht.clone().map(|d| {
                let up: Arc<dyn Fn() -> bool + Send + Sync> =
                    Arc::new(move || d.lock().is_ok_and(|s| s.is_some()));
                up
            }),
            search: self.handles.search.clone().map(SearchHandle::serving),
        }
    }

    /// Serves HTTP until `signal` resolves, then shuts everything down.
    pub async fn run(
        self,
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), BoxError> {
        let Services {
            api,
            export_adult,
            http,
            events,
            handles,
            listener,
            system,
            updates,
        } = self;
        // Exports and other background jobs (§9): one registry, closed at
        // shutdown so open downloads end rather than hold it.
        let jobs = Arc::new(lodestar_api::jobs::Jobs::default());
        let logs = crate::logging::source();
        let app = http::router(http, api)
            .layer(Extension(events.clone()))
            .layer(Extension(logs.clone()))
            .layer(Extension(jobs.clone()))
            .layer(Extension(export_adult))
            .layer(Extension(system))
            .layer(Extension(updates))
            .into_make_service_with_connect_info::<SocketAddr>();
        // Open streams never end on their own: closing the hub ends them,
        // and closing the jobs ends open exports, so the graceful
        // shutdown doesn't wait on them.
        let hub = events.clone();
        let (stopping_tx, stopping) = watch::channel(false);
        let signal = async move {
            signal.await;
            hub.close();
            jobs.close();
            logs.close();
            let _ = stopping_tx.send(true);
        };
        let serve = axum::serve(listener, app)
            .with_graceful_shutdown(signal)
            .into_future();
        // A client holding a download open without reading would keep the
        // graceful shutdown waiting forever, and nothing below would run.
        // Past the bound its connections are dropped: the tasks axum
        // spawned for them run on, detached, until the runtime ends, and
        // hold only the router's state, nothing `shutdown` waits on.
        let served = bounded(serve, stopping, SHUTDOWN_GRACE)
            .await
            .unwrap_or(Ok(()));
        // Saved and logged whatever `serve` returned: no error skips the
        // save or the required `app.stop`.
        events.close();
        let stopped = handles.shutdown().await;
        // Required, and last: pending fold summaries are written first.
        lodestar_log::log_event!(
            crate::logging::events::APP_STOP,
            clean = stopped.is_ok() && served.is_ok()
        );
        served?;
        stopped
    }
}

/// How long the graceful shutdown waits for open connections once the
/// signal fires (B-log1b, §2.19).
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// How long the runtime waits, once serving has ended, for blocking tasks
/// still running (a stuck read): the exit, and the Windows service's stop,
/// never hang on one. With [`SHUTDOWN_GRACE`] and the saves, inside the
/// update supervisor's and the SCM's 30 s.
pub const RUNTIME_GRACE: Duration = Duration::from_secs(5);

/// `serve`'s result, or `None` once `grace` has passed since `stopping`
/// turned true: the future is dropped then, and `app.shutdown_forced` is
/// logged.
async fn bounded<T>(
    serve: impl Future<Output = T>,
    stopping: watch::Receiver<bool>,
    grace: Duration,
) -> Option<T> {
    tokio::select! {
        served = serve => Some(served),
        () = forced_after(stopping, grace) => {
            lodestar_log::log_event!(
                crate::logging::events::APP_SHUTDOWN_FORCED,
                waited_s = grace.as_secs()
            );
            None
        }
    }
}

/// Resolves `grace` after `stopping` turns true; never if it doesn't.
async fn forced_after(mut stopping: watch::Receiver<bool>, grace: Duration) {
    while !*stopping.borrow_and_update() {
        if stopping.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
    tokio::time::sleep(grace).await;
}

impl Handles {
    /// Stops every service in order: the blocklist refresher, the
    /// pipeline (which saves the queue and DHT state and commits the last
    /// batch), then the indexer, then the percolator.
    pub async fn shutdown(self) -> Result<(), BoxError> {
        lodestar_log::log_event!(crate::logging::events::APP_STOPPING);
        // First, and never waits on a sample under way.
        let (stop, sampler) = self.storage;
        let _ = stop.send(true);
        let _ = sampler.await;
        // A check under way is abandoned: it writes only after it's
        // verified, by rename, so nothing is left half-written.
        if let Some((stop, task)) = self.updates {
            let _ = stop.send(true);
            task.abort();
            let _ = task.await;
        }
        let (stop, refresher) = self.subscriptions;
        let _ = stop.send(true);
        let _ = refresher.await;
        let mut store = self.pipeline.shutdown().await?;
        // A failed snapshot only costs a rebuild from SQLite at the next
        // start; the indexer still gets to stop cleanly.
        let (hot, hot_dir) = (self.hot, self.hot_dir);
        match tokio::task::spawn_blocking(move || {
            lodestar_pipeline::save_hot(&mut store, &hot, &hot_dir)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => lodestar_log::log_event!(
                crate::logging::events::APP_HOT_SAVE_FAILED,
                error = lodestar_log::Error::new(&e)
            ),
            Err(e) => lodestar_log::log_event!(
                crate::logging::events::APP_HOT_SAVE_FAILED,
                error = lodestar_log::Error::new(&e)
            ),
        }
        if let Some(search) = self.search {
            // After the writer, so the last batch gets indexed too. Other
            // handles (requests still finishing) may hold it: stopping
            // works through a shared one.
            tokio::task::spawn_blocking(move || search.stop()).await??;
        }
        // After the indexer's last commit, which it then records.
        let (stop, recorder) = self.insights;
        let _ = stop.send(true);
        let _ = recorder.await;
        if let Some((stop, task)) = self.percolator {
            // After the indexer, whose last commit it may still record.
            let _ = stop.send(true);
            let _ = task.await;
        }
        lodestar_log::log_event!(crate::logging::events::APP_STOPPED_CLEANLY);
        Ok(())
    }
}

/// The key that seals quarantined names (§20.3), created on first use.
/// It lives beside the database, not in it, so a copy of the database
/// alone doesn't reveal names; on Unix only the owner can read it.
fn seal_key(path: &Path) -> Result<[u8; 32], BoxError> {
    if let Some(key) = crate::filter_load::read_seal_key(path)? {
        return Ok(key);
    }
    let key = Sealer::generate_key()?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .map_err(|e| format!("creating {}: {e}", path.display()))?;
    file.write_all(&key)?;
    file.sync_all()?;
    Ok(key)
}

/// The API's interest hints go to the scrape scheduler; its searches to
/// the Serve rollups (§10.9.5).
#[derive(Debug)]
struct Events {
    health: Option<HealthHandle>,
    rollups: Arc<Rollups>,
}

impl lodestar_api::ApiEvents for Events {
    fn interest(&self, id: i64) {
        if let Some(h) = &self.health {
            h.interest(id);
        }
    }

    fn searched(&self, took: Duration, total: Option<usize>) {
        let now = crate::unix_now();
        let outcome = match total {
            Some(0) => 1,
            Some(_) => 0,
            None => 2,
        };
        self.rollups.add_dim(now, metric::SEARCHES, 0, outcome, 1);
        self.rollups.add_dim(
            now,
            metric::SEARCH_LATENCY,
            0,
            metric::latency_bucket(took),
            1,
        );
    }
}

/// The crawler gate's searchable count (CR-gate-metrics) follows the
/// serving index's commits, which outlive a swap. The pipeline was
/// started with `gate_follows_index` set from the same index, so the
/// writer has handed ids over since its first commit.
fn follow_gate(gate: Arc<GateStats>, hook: &CommitHook) {
    hook.add(move |c| gate.searchable(c.max_torrent_id, c.at as i64));
}

#[cfg(test)]
mod gate_tests {
    use std::net::SocketAddr;
    use std::time::Instant;

    use lodestar_metainfo::{Info, InfoHash};
    use lodestar_peer_wire::{FailureKind, FetchStats, Fetched};
    use lodestar_pipeline::PipelineConfig;
    use lodestar_pipeline::gate::Origin;
    use lodestar_sources::{
        BoxFuture, Consent, Fetcher, Found, Kind, Manifest, Network, PluginError, Priority, Risk,
        SourceCtx,
    };
    use lodestar_store::StoreConfig;

    use super::*;

    static ONE: Manifest = Manifest {
        id: "one",
        name: "One key",
        version: "1",
        kinds: &[Kind::Discovery],
        networks: &[Network::Bt],
        endpoints: &[],
        risk: Risk::Low,
        consent: Consent::None,
        default_priority: Priority::Normal,
        enabled_by_default: true,
    };

    /// Reports one key, with a peer, then waits.
    struct One(InfoHash);

    impl SourcePlugin for One {
        fn manifest(&self) -> &Manifest {
            &ONE
        }

        fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
            let found =
                Found::key(self.0, 0).with_peers(vec![SocketAddr::from(([10, 0, 0, 1], 1))]);
            Box::pin(async move {
                ctx.emitter
                    .emit(found)
                    .await
                    .map_err(|_| PluginError("closed".into()))?;
                ctx.shutdown.requested().await;
                Ok(())
            })
        }
    }

    /// Every peer serves the same verified dictionary.
    struct Serves(Vec<u8>);

    impl Fetcher for Serves {
        fn fetch(
            &self,
            _peer: SocketAddr,
            _key: InfoHash,
        ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
            let info = self.0.clone();
            Box::pin(async move {
                Ok(Fetched {
                    info,
                    v2: false,
                    encrypted: false,
                    pex: Vec::new(),
                    client: None,
                })
            })
        }
    }

    /// The wiring `start` uses: a network-new torrent reaches the gate's
    /// searchable count through a real serving index's commit, so a
    /// broken hook can't leave the gate's numerator at 0 unnoticed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_serving_index_commit_reaches_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        let serving = Serving::open_or_rebuild(
            &dir.path().join("index"),
            &db,
            ServingConfig {
                live: LiveConfig {
                    heap_bytes: 15 << 20,
                    poll: Duration::from_millis(20),
                    commit_every: Duration::from_millis(50),
                    ..LiveConfig::default()
                },
                ..ServingConfig::default()
            },
        )
        .unwrap();
        // A single-file info dictionary, bencoded by hand (keys sorted).
        let mut raw =
            b"d6:lengthi1048576e4:name14:Gate.Probe.mkv12:piece lengthi262144e6:pieces80:".to_vec();
        raw.extend([0u8; 80]);
        raw.push(b'e');
        let key = Info::parse(&raw).unwrap().primary_hash();
        let pipeline = Pipeline::start_with(
            PipelineConfig {
                flush_every: Duration::from_millis(100),
                batch_wait: Duration::from_millis(10),
                gate_follows_index: true,
                ..PipelineConfig::default()
            },
            store,
            vec![Arc::new(One(key))],
            Arc::new(Serves(raw)),
        )
        .await
        .unwrap();
        let gate = pipeline.monitor().gate().clone();
        follow_gate(gate.clone(), serving.commit_hook());

        let deadline = Instant::now() + Duration::from_secs(20);
        while gate.searchable_new(Origin::Network) < 1 {
            assert!(
                Instant::now() < deadline,
                "not searchable: {} new verified, {} indexed, watermark {}",
                gate.new_verified(Origin::Network),
                serving.stats().indexed.load(Ordering::Relaxed),
                serving.stats().watermark.load(Ordering::Relaxed),
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(gate.new_verified(Origin::Network), 1);
        assert_eq!(gate.latency.cumulative().last().copied(), Some(1));
        let body = crate::metrics::render(&pipeline.monitor(), None, &FetchStats::default());
        assert!(
            body.contains("lodestar_searchable_new_total{origin=\"network\"} 1\n"),
            "{body}"
        );
        pipeline.shutdown().await.unwrap();
        serving.shutdown().unwrap();
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    /// Every case finishes well inside this; it only keeps a broken one
    /// from hanging the suite.
    const DEADLINE: Duration = Duration::from_secs(10);

    /// A serve future that never ends (a stalled client's) is dropped
    /// once the bound has passed after the signal.
    #[tokio::test]
    async fn a_serve_that_never_ends_is_dropped_after_the_bound() {
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let got = tokio::time::timeout(
            DEADLINE,
            bounded(std::future::pending::<()>(), rx, Duration::from_millis(50)),
        )
        .await
        .expect("the bound ended it");
        assert_eq!(got, None);
    }

    /// A serve future that ends wins, whatever the bound.
    #[tokio::test]
    async fn a_serve_that_ends_returns_its_result() {
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let got = tokio::time::timeout(DEADLINE, bounded(async { 7 }, rx, DEADLINE * 2))
            .await
            .unwrap();
        assert_eq!(got, Some(7));
    }

    /// Before the signal the bound never runs, even at zero: only `serve`
    /// can end the race.
    #[tokio::test]
    async fn without_the_signal_the_bound_never_runs() {
        let (_tx, rx) = watch::channel(false);
        let (done, ended) = tokio::sync::oneshot::channel();
        let serve = async move { ended.await.unwrap() };
        let race = tokio::spawn(bounded(serve, rx, Duration::ZERO));
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        done.send("served").unwrap();
        let got = tokio::time::timeout(DEADLINE, race).await.unwrap().unwrap();
        assert_eq!(got, Some("served"));
    }
}
