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

use axum::Extension;
use lodestar_api::dto::events::CrawlerEvent;
use lodestar_api::events::{EventHub, EventSources, EventsConfig};
use lodestar_filter::Sealer;
use lodestar_filter::lists::SharedIps;
use lodestar_pipeline::{Control, DhtSource, Pipeline};
use lodestar_search::{LiveConfig, SearchHandle, Serving, ServingConfig};
use lodestar_sources::SourcePlugin;
use lodestar_store::Store;
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
}

/// What has to be stopped on the way out.
pub struct Handles {
    /// The blocklist refresher (§20.13) and its stop signal.
    subscriptions: (watch::Sender<bool>, JoinHandle<()>),
    pipeline: Pipeline,
    /// The index, its live indexer and any rebuild (§8).
    search: Option<Arc<Serving>>,
    /// Insights' searchable-time recorder and its stop signal.
    insights: (watch::Sender<bool>, JoinHandle<()>),
    /// Saved searches → alerts (B9), while search is on.
    percolator: Option<(watch::Sender<bool>, JoinHandle<()>)>,
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
        // Subscribed IP lists (§20.13): one shared set, consulted by the
        // fetch pool and the DHT before talking to anyone.
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
        let heap = config.search.heap_mib.max(16) << 20;
        let filter = pipeline_config.filter.clone();
        let subs_config = config.subscriptions.clone();
        let blocklist = config.blocklist_files()?;
        let subs_ips = ips.clone();
        let (store, control, user_data, reader, serving, percolation) =
            tokio::task::spawn_blocking(move || {
                let store = Store::open(&db, store_config.clone())?;
                // Settings and lists change through a second connection, so
                // they never wait behind the ingest writer for long.
                let mut control = Store::open(&db, store_config.clone())?;
                subscriptions::sync_config(&mut control, &subs_config)?;
                // Before the pipeline loads the floor from the store.
                floor::sync_files(&mut control, &blocklist, crate::unix_now())?;
                floor::report(&control)?;
                subscriptions::apply(&control, &filter, &subs_ips)?;
                let user_data = Store::open(&db, store_config.clone())?;
                let reader = Store::open_reader(&db, store_config.clone())?;
                // The percolator records alerts through its own connection.
                let percolation = search
                    .then(|| Store::open(&db, store_config.clone()))
                    .transpose()?;
                // The index this build serves (rebuilt in the background from
                // an older one if need be); its indexer follows the store
                // through its own connection.
                let serving = if search {
                    let config = ServingConfig {
                        live: LiveConfig {
                            heap_bytes: heap,
                            ..LiveConfig::default()
                        },
                        store: store_config,
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
                Ok::<_, BoxError>((store, control, user_data, reader, serving, percolation))
            })
            .await??;

        let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
        let mut dht_stats = None;
        let mut dht_source = None;
        if config.dht.enabled {
            let mut dht_config = config.dht_config();
            dht_config.deny = Some(lodestar_dht::node::Deny(denied.clone()));
            dht_config.paused = crawl_settings.paused;
            let dht = Arc::new(DhtSource::new(dht_config));
            dht_stats = Some(dht.stats_handle());
            settings::crawler::follow_intensity(settings.subscribe(), dht.node_handle());
            dht_source = Some(dht.clone());
            plugins.push(dht);
        }
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
            tracing::warn!(
                "{} is set: trusted-proxy sign-in is off for this run",
                lodestar_api::auth::PROXY_AUTH_ENV
            );
        }
        if let Some(token) = auth.setup_token() {
            // §12: whoever can read the logs claims the instance.
            tracing::warn!(
                "first run: open {} and enter setup token {token} to create the admin password",
                ops::ui_address(config.http.bind)
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
        // Insights (B11a): searchable time from the index's commits.
        let insights = Arc::new(insights::Insights::new(pipeline.monitor()));
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
        let ops = Arc::new(ops::Ops::new(
            crawl,
            settings.subscribe(),
            pipeline.monitor(),
            search_state.clone(),
            config.data_dir.clone(),
        ));
        let api = lodestar_api::ApiState {
            auth,
            index: search.clone(),
            reader: reader.clone(),
            filters: Some(filters),
            settings: Some(settings.clone()),
            ops: Some(ops),
            user_data: Some(Arc::new(Mutex::new(user_data))),
            adult: settings::content::follow(settings.subscribe()),
            insights: Some(insights),
        };
        settings::log::follow(settings.subscribe());
        let http = http::AppState {
            monitor: pipeline.monitor(),
            dht: dht_stats,
            fetch: fetch_stats,
            reader,
            search: search_state,
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
        let system = system::System::start(config, bound, &api, &settings, control, &events);
        tracing::info!(
            "UI on {}, status page on /status (data in {})",
            ops::ui_address(served),
            config.data_dir.display()
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
                insights: (stop_insights, recorder),
                percolator,
            },
            listener,
            system,
        })
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
        } = self;
        // Exports and other background jobs (§9): one registry, closed at
        // shutdown so open downloads end rather than hold it.
        let jobs = Arc::new(lodestar_api::jobs::Jobs::default());
        let app = http::router(http, api)
            .layer(Extension(events.clone()))
            .layer(Extension(jobs.clone()))
            .layer(Extension(export_adult))
            .layer(Extension(system))
            .into_make_service_with_connect_info::<SocketAddr>();
        // Open streams never end on their own: closing the hub ends them,
        // and closing the jobs ends open exports, so the graceful
        // shutdown doesn't wait on them.
        let hub = events.clone();
        let signal = async move {
            signal.await;
            hub.close();
            jobs.close();
        };
        axum::serve(listener, app)
            .with_graceful_shutdown(signal)
            .await?;
        events.close();
        handles.shutdown().await
    }
}

impl Handles {
    /// Stops every service in order: the blocklist refresher, the
    /// pipeline (which saves the queue and DHT state and commits the last
    /// batch), then the indexer, then the percolator.
    pub async fn shutdown(self) -> Result<(), BoxError> {
        tracing::info!("shutting down: saving the queue and DHT state");
        let (stop, refresher) = self.subscriptions;
        let _ = stop.send(true);
        let _ = refresher.await;
        self.pipeline.shutdown().await?;
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
        tracing::info!("stopped cleanly");
        Ok(())
    }
}

/// The key that seals quarantined names (§20.3), created on first use.
/// It lives beside the database, not in it, so a copy of the database
/// alone doesn't reveal names; on Unix only the owner can read it.
fn seal_key(path: &Path) -> Result<[u8; 32], BoxError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            return bytes
                .try_into()
                .map_err(|_| format!("{}: not a 32-byte key", path.display()).into());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("reading {}: {e}", path.display()).into()),
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
