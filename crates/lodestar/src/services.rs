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

use lodestar_filter::Sealer;
use lodestar_filter::lists::SharedIps;
use lodestar_pipeline::{DhtSource, Pipeline};
use lodestar_search::{LiveConfig, SearchHandle, Serving, ServingConfig};
use lodestar_sources::SourcePlugin;
use lodestar_store::Store;
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
    /// The index, its live indexer and any rebuild (§8).
    search: Option<Arc<Serving>>,
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
        let (store, control, reader, serving) = tokio::task::spawn_blocking(move || {
            let store = Store::open(&db, store_config.clone())?;
            // Settings and lists change through a second connection, so
            // they never wait behind the ingest writer for long.
            let mut control = Store::open(&db, store_config.clone())?;
            subscriptions::sync_config(&mut control, &subs_config)?;
            // Before the pipeline loads the floor from the store.
            floor::sync_files(&mut control, &blocklist, crate::unix_now())?;
            floor::report(&control)?;
            subscriptions::apply(&control, &filter, &subs_ips)?;
            let reader = Store::open_reader(&db, store_config.clone())?;
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
            Ok::<_, BoxError>((store, control, reader, serving))
        })
        .await??;

        let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
        let mut dht_stats = None;
        if config.dht.enabled {
            let mut dht_config = config.dht_config();
            dht_config.deny = Some(lodestar_dht::node::Deny(denied.clone()));
            let dht = Arc::new(DhtSource::new(dht_config));
            dht_stats = Some(dht.stats_handle());
            plugins.push(dht);
        }
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
        let api = lodestar_api::ApiState {
            auth,
            index: search.clone(),
            reader: reader.clone(),
            filters: Some(filters),
        };
        let http = http::AppState {
            monitor: pipeline.monitor(),
            dht: dht_stats,
            fetch: fetch_stats,
            reader,
            search: serving.as_ref().map(|s| http::SearchState {
                index: SearchHandle::serving(s.clone()),
                stats: s.stats(),
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
                search: serving,
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
        self.pipeline.shutdown().await?;
        if let Some(search) = self.search {
            // After the writer, so the last batch gets indexed too. Other
            // handles (requests still finishing) may hold it: stopping
            // works through a shared one.
            tokio::task::spawn_blocking(move || search.stop()).await??;
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
