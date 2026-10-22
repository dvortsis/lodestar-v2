//! Builds and wires every long-running service of `lodestar serve`: the
//! store and its readers, the blocklist subscriptions, the pipeline, the
//! live indexer, the filters API, the API state and the HTTP server. `main.rs` parses the command line and calls
//! [`Services::start`]; [`Services::run`] serves until a signal, then
//! shuts down in order.
//!
//! A new service is built in [`Services::start`], keeps its handle in
//! [`Handles`], and stops in [`Handles::shutdown`].

use std::future::Future;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_filter::Sealer;
use lodestar_filter::lists::SharedIps;
use lodestar_pipeline::health::HealthHandle;
use lodestar_pipeline::{DhtSource, HealthSetup, Pipeline, Rollups, metric};
use lodestar_search::{LiveConfig, LiveIndexer, SearchIndex};
use lodestar_sources::SourcePlugin;
use lodestar_store::{HotColumns, Store, hot};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::{floor, http, subscriptions};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Everything `serve` runs, started and wired.
pub struct Services {
    /// The API's state (auth, index, reader), as `/api/v1` sees it.
    pub api: lodestar_api::ApiState,
    /// The status page's and metrics' state.
    pub http: http::AppState,
    pub handles: Handles,
    listener: TcpListener,
}

/// What has to be stopped on the way out.
pub struct Handles {
    /// The blocklist refresher (§20.13) and its stop signal.
    subscriptions: (watch::Sender<bool>, JoinHandle<()>),
    pipeline: Pipeline,
    live: Option<LiveIndexer>,
    /// Swarm stats (§7), snapshotted to `hot_dir` after the pipeline stops.
    hot: Arc<HotColumns>,
    hot_dir: std::path::PathBuf,
}

impl Services {
    /// Opens the data directory and starts every service. The HTTP
    /// listener is bound but serves nothing until [`Services::run`].
    pub async fn start(config: &Config) -> Result<Services, BoxError> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|e| format!("creating {}: {e}", config.data_dir.display()))?;
        let mut pipeline_config = config.pipeline_config()?;
        let key = seal_key(&config.data_dir.join("seal.key"))?;
        pipeline_config.sealer = Some(Arc::new(Sealer::new(&key)));
        let health = config.health_config()?;
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
        let index_path = config.index_path();
        let heap = config.search.heap_mib.max(16) << 20;
        let filter = pipeline_config.filter.clone();
        let subs_config = config.subscriptions.clone();
        let blocklist = config.blocklist_files()?;
        let subs_ips = ips.clone();
        let hot_dir = config.data_dir.join("hot");
        let weights = config.ranking.weights();
        let suspect_penalty = config.ranking.suspect.clamp(0.0, 1.0);
        let (db2, store_config2, hot_dir2) = (db.clone(), store_config.clone(), hot_dir.clone());
        let (store, control, reader, live, hot) = tokio::task::spawn_blocking(move || {
            let (db, store_config) = (db2, store_config2);
            let mut store = Store::open(&db, store_config.clone())?;
            // Settings and lists change through a second connection, so
            // they never wait behind the ingest writer for long.
            let mut control = Store::open(&db, store_config.clone())?;
            subscriptions::sync_config(&mut control, &subs_config)?;
            // Before the pipeline loads the floor from the store.
            floor::sync_files(&mut control, &blocklist, crate::unix_now())?;
            floor::report(&control)?;
            subscriptions::apply(&control, &filter, &subs_ips)?;
            // Swarm stats for search and the writer (§7): last run's
            // snapshot, or rebuilt from SQLite after a crash.
            let started = std::time::Instant::now();
            let (hot, from_snapshot) = lodestar_pipeline::open_hot(&mut store, &hot_dir2)?;
            tracing::info!(
                torrents = hot.len(),
                ms = started.elapsed().as_millis() as u64,
                "hot columns {}",
                if from_snapshot {
                    "loaded"
                } else {
                    "rebuilt from the store"
                }
            );
            let hot = Arc::new(hot);
            let reader = Store::open_reader(&db, store_config.clone())?;
            // The indexer follows the store through its own connection.
            let live = if search {
                let suspect = hot.clone();
                let index = SearchIndex::open(&index_path)?
                    .with_hot(hot.clone())
                    .with_weights(weights)
                    // §8: a fake-seeding swarm ranks well below its claim.
                    .with_penalty(Arc::new(move |id| {
                        if suspect.flags(id) & hot::flags::SUSPECT != 0 {
                            suspect_penalty
                        } else {
                            1.0
                        }
                    }));
                let live_reader = Store::open_reader(&db, store_config)?;
                let config = LiveConfig {
                    heap_bytes: heap,
                    ..LiveConfig::default()
                };
                Some((
                    index.clone(),
                    LiveIndexer::spawn(index, live_reader, config)?,
                ))
            } else {
                None
            };
            Ok::<_, BoxError>((store, control, reader, live, hot))
        })
        .await??;

        let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
        let mut dht_stats = None;
        let mut dht_node = None;
        if config.dht.enabled {
            let mut dht_config = config.dht_config();
            dht_config.deny = Some(lodestar_dht::node::Deny(denied.clone()));
            let dht = Arc::new(DhtSource::new(dht_config));
            dht_stats = Some(dht.stats_handle());
            dht_node = Some(dht.node_handle());
            plugins.push(dht);
        }
        pipeline_config.hot = Some(hot.clone());
        pipeline_config.health = health.map(|(cfg, trackers)| HealthSetup {
            config: lodestar_pipeline::health::HealthConfig {
                deny: Some(lodestar_tracker_scrape::Deny(denied.clone())),
                ..cfg
            },
            trackers,
            db: db.clone(),
            store: store_config.clone(),
            dht: dht_node,
        });
        let auth = Arc::new(
            lodestar_api::Auth::load(
                &config.data_dir.join("auth.json"),
                config.http.secure_cookies,
            )
            .map_err(|e| format!("loading auth: {e}"))?,
        );
        if let Some(token) = auth.setup_token() {
            // §12: whoever can read the logs claims the instance.
            tracing::warn!(
                "first run: open http://{}/ and enter setup token {token} to create the admin password",
                config.http.bind
            );
        }
        let filter = pipeline_config.filter.clone();
        let held = pipeline_config.held.clone();
        let sealer = pipeline_config.sealer.clone();
        let reenrich = pipeline_config.reenrich.clone();
        let pipeline = Pipeline::start(pipeline_config, store, plugins).await?;
        let control = Arc::new(Mutex::new(control));
        let filters = Arc::new(lodestar_api::filters::FiltersApi {
            store: control.clone(),
            engine: filter.clone(),
            held,
            sealer,
            ips: ips.clone(),
            index: live.as_ref().map(|(index, _)| index.clone()),
            reenrich,
            jobs: Default::default(),
        });
        let (stop_subscriptions, subscriptions_stopped) = watch::channel(false);
        let refresher = tokio::spawn(subscriptions::run(
            control.clone(),
            subscriptions::Limits::from_config(&config.subscriptions),
            filter,
            ips,
            subscriptions_stopped,
        ));
        let reader = Arc::new(Mutex::new(reader));
        let monitor = pipeline.monitor();
        let api = lodestar_api::ApiState {
            auth,
            index: live.as_ref().map(|(index, _)| index.clone()),
            reader: reader.clone(),
            filters: Some(filters),
            hot: Some(hot.clone()),
            events: Some(Arc::new(Events {
                health: monitor.health().cloned(),
                rollups: monitor.rollups().clone(),
            })),
        };
        let http = http::AppState {
            monitor: pipeline.monitor(),
            dht: dht_stats,
            fetch: fetch_stats,
            reader,
            search: live.as_ref().map(|(index, l)| http::SearchState {
                index: index.clone(),
                stats: l.stats(),
            }),
        };
        let listener = TcpListener::bind(config.http.bind)
            .await
            .map_err(|e| format!("binding {}: {e}", config.http.bind))?;
        tracing::info!(
            "UI on http://{}/, status page on /status (data in {})",
            config.http.bind,
            config.data_dir.display()
        );
        Ok(Services {
            api,
            http,
            handles: Handles {
                subscriptions: (stop_subscriptions, refresher),
                pipeline,
                live: live.map(|(_, live)| live),
                hot,
                hot_dir,
            },
            listener,
        })
    }

    /// Serves HTTP until `signal` resolves, then shuts everything down.
    pub async fn run(
        self,
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), BoxError> {
        let Services {
            api,
            http,
            handles,
            listener,
        } = self;
        let app = http::router(http, api).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app)
            .with_graceful_shutdown(signal)
            .await?;
        handles.shutdown().await
    }
}

impl Handles {
    /// Stops every service in order: the blocklist refresher, the
    /// pipeline (which saves the queue and DHT state and commits the last
    /// batch), then the indexer.
    pub async fn shutdown(self) -> Result<(), BoxError> {
        tracing::info!("shutting down: saving the queue and DHT state");
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
            Ok(Err(e)) => tracing::error!("saving hot columns: {e}"),
            Err(e) => tracing::error!("saving hot columns: {e}"),
        }
        if let Some(live) = self.live {
            // After the writer, so the last batch gets indexed too.
            tokio::task::spawn_blocking(move || live.stop()).await??;
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
