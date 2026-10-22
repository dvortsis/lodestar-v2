//! Builds and wires every long-running service of `lodestar serve`: the
//! store and its readers, the pipeline, the live indexer, the API state
//! and the HTTP server. `main.rs` parses the command line and calls
//! [`Services::start`]; [`Services::run`] serves until a signal, then
//! shuts down in order.
//!
//! A new service is built in [`Services::start`], keeps its handle in
//! [`Handles`], and stops in [`Handles::shutdown`].

use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lodestar_filter::Sealer;
use lodestar_pipeline::{DhtSource, Pipeline};
use lodestar_search::{LiveConfig, LiveIndexer, SearchIndex};
use lodestar_sources::SourcePlugin;
use lodestar_store::Store;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::http;

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
    pipeline: Pipeline,
    live: Option<LiveIndexer>,
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
        let db = config.db_path();
        let store_config = config.store_config();
        let search = config.search.enabled;
        let index_path = config.index_path();
        let heap = config.search.heap_mib.max(16) << 20;
        let (store, reader, live) = tokio::task::spawn_blocking(move || {
            let store = Store::open(&db, store_config.clone())?;
            let reader = Store::open_reader(&db, store_config.clone())?;
            // The indexer follows the store through its own connection.
            let live = if search {
                let index = SearchIndex::open(&index_path)?;
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
            Ok::<_, BoxError>((store, reader, live))
        })
        .await??;

        let mut plugins: Vec<Arc<dyn SourcePlugin>> = Vec::new();
        let mut dht_stats = None;
        if config.dht.enabled {
            let dht = Arc::new(DhtSource::new(config.dht_config()));
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
        let pipeline = Pipeline::start(pipeline_config, store, plugins).await?;
        let reader = Arc::new(Mutex::new(reader));
        let api = lodestar_api::ApiState {
            auth,
            index: live.as_ref().map(|(index, _)| index.clone()),
            reader: reader.clone(),
        };
        let http = http::AppState {
            monitor: pipeline.monitor(),
            dht: dht_stats,
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
            "status page on http://{}/ (data in {})",
            config.http.bind,
            config.data_dir.display()
        );
        Ok(Services {
            api,
            http,
            handles: Handles {
                pipeline,
                live: live.map(|(_, live)| live),
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
    /// Stops every service in order: the pipeline (which saves the queue
    /// and DHT state and commits the last batch), then the indexer.
    pub async fn shutdown(self) -> Result<(), BoxError> {
        tracing::info!("shutting down: saving the queue and DHT state");
        self.pipeline.shutdown().await?;
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
