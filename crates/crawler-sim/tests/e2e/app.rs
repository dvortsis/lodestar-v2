//! One Lodestar install, assembled from the production parts the way
//! `crates/lodestar/src/services.rs` assembles them, minus the HTTP
//! listener: the real store in a temp data dir, the pipeline with the
//! real DHT plugin and the peer-wire fetcher, the serving index with its
//! live indexer, and the `/api/v1` router, called in process.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::dto::search::SearchResponse;
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_dht::node::Config;
use lodestar_pipeline::{DhtSource, Pipeline, PipelineConfig};
use lodestar_search::{LiveConfig, SearchHandle, Serving, ServingConfig};
use lodestar_store::{Store, StoreConfig};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// A data dir under the system temp dir, removed on drop.
pub struct DataDir(PathBuf);

impl DataDir {
    pub fn new() -> DataDir {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lodestar-e2e-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp data dir");
        DataDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for DataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The crawler's DHT node: production settings, except that it binds
/// loopback, bootstraps from the test network, may talk to private
/// addresses, and samples at once (production suppresses sampling for
/// 6 h after a start without a restart proof, CR-0d).
pub fn crawler_config(bootstrap: String) -> Config {
    Config {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        bootstrap: vec![bootstrap],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        state_path: None,
        ..Config::default()
    }
}

pub fn pipeline_config() -> PipelineConfig {
    PipelineConfig {
        // Loopback peers are private addresses.
        fetch: lodestar_peer_wire::FetchOptions {
            allow_private: true,
            ..Default::default()
        },
        ..PipelineConfig::default()
    }
}

pub struct App {
    pub pipeline: Pipeline,
    pub dht: Arc<DhtSource>,
    serving: Arc<Serving>,
    api: Router,
    cookie: String,
    db: PathBuf,
    _dir: DataDir,
}

impl App {
    /// The store, index and API, ready before the crawler starts.
    pub fn prepare() -> Prepared {
        let dir = DataDir::new();
        let db = dir.path().join("lodestar.db");
        let store = Store::open(&db, StoreConfig::default()).expect("open store");
        let serving = Arc::new(
            Serving::open_or_rebuild(
                &dir.path().join("index"),
                &db,
                ServingConfig {
                    live: LiveConfig {
                        // The configurable heap, at a test's size; the
                        // poll and commit cadence are production's.
                        heap_bytes: 64 << 20,
                        ..LiveConfig::default()
                    },
                    ..ServingConfig::default()
                },
            )
            .expect("open index"),
        );
        let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).expect("auth"));
        let reader = Store::open_reader(&db, StoreConfig::default()).expect("reader");
        let api = Router::new().nest(
            "/api/v1",
            router(ApiState {
                auth: auth.clone(),
                index: Some(SearchHandle::serving(serving.clone())),
                reader: Arc::new(Mutex::new(reader)),
                filters: None,
                settings: None,
                adult: AdultVisibility::follow(|| false),
                ops: None,
                insights: None,
                user_data: None,
                hot: None,
                events: None,
            }),
        );
        Prepared {
            store,
            serving,
            api,
            auth,
            db,
            dir,
        }
    }

    /// `GET /api/v1/search?q=…&limit=…`, as the signed-in admin.
    pub async fn search(&self, q: &str, limit: usize) -> SearchResponse {
        assert!(
            q.bytes().all(|b| b.is_ascii_alphanumeric() || b == b' '),
            "only words and spaces need no URL encoding: {q}"
        );
        let q = q.replace(' ', "+");
        let uri = format!("/api/v1/search?q={q}&limit={limit}");
        let req = Request::get(&uri)
            .header(header::COOKIE, &self.cookie)
            .body(Body::empty())
            .expect("request");
        let (status, bytes, _) = call(&self.api, req).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{uri}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).expect("a search response")
    }

    /// Whether the store holds `hash`, through a connection of its own.
    pub fn stored(&self, hash: &[u8; 20]) -> bool {
        let store = Store::open_reader(&self.db, StoreConfig::default()).expect("reader");
        store
            .find(&lodestar_metainfo::InfoHash(*hash))
            .expect("find")
            .is_some()
    }

    pub async fn shutdown(self) {
        let store = self.pipeline.shutdown().await.expect("pipeline stops");
        drop(store);
        drop(self.api);
        let serving = Arc::try_unwrap(self.serving).expect("the only handle left");
        serving.shutdown().expect("index stops");
    }
}

pub struct Prepared {
    store: Store,
    serving: Arc<Serving>,
    api: Router,
    auth: Arc<Auth>,
    db: PathBuf,
    dir: DataDir,
}

impl Prepared {
    /// Signs in (first-run setup), then starts the pipeline with the DHT
    /// plugin joining the network at `bootstrap`.
    pub async fn start(self, bootstrap: String) -> App {
        let cookie = setup(&self.api, &self.auth).await;
        let dht = Arc::new(DhtSource::new(crawler_config(bootstrap)));
        let pipeline = Pipeline::start(pipeline_config(), self.store, vec![dht.clone()])
            .await
            .expect("pipeline starts");
        App {
            pipeline,
            dht,
            serving: self.serving,
            api: self.api,
            cookie,
            db: self.db,
            _dir: self.dir,
        }
    }
}

async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, Vec<u8>, Option<String>) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
    let resp = app.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .expect("body");
    (status, bytes.to_vec(), cookie)
}

async fn setup(app: &Router, auth: &Auth) -> String {
    let token = auth.setup_token().expect("a new install has a setup token");
    let body = serde_json::json!({
        "token": token,
        "password": PASSWORD,
        "notice_version": lodestar_api::legal::NOTICE_VERSION,
    });
    let req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let (status, bytes, cookie) = call(app, req).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    cookie.expect("a session cookie")
}
