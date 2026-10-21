//! The `lodestar` binary (architecture §11): `serve` (the default) runs the
//! crawler with its status page and metrics; `healthcheck` probes a
//! running instance for Docker's `HEALTHCHECK`.

mod config;
mod http;
mod metrics;

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_pipeline::{DhtSource, Pipeline};
use lodestar_search::{LiveConfig, LiveIndexer, SearchIndex};
use lodestar_sources::SourcePlugin;
use lodestar_store::Store;

use crate::config::Config;

// musl's allocator is a severe bottleneck for multithreaded Rust (§11).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "\
Usage: lodestar [serve|healthcheck] [--config <path>]

Commands:
  serve        Run the crawler, status page and metrics (default)
  healthcheck  Exit 0 if the local instance answers /healthz, 1 otherwise

Options:
  --config <path>  config.toml (default: $LODESTAR_DATA_DIR/config.toml or
                   data/config.toml, if present)
  -V, --version    Print the version
  -h, --help       Print this help

Environment: LODESTAR_DATA_DIR, LODESTAR_HTTP_BIND, LODESTAR_DHT_PORT,
LODESTAR_DHT_ENABLED, LODESTAR_INTENSITY, LODESTAR_LOG";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Serve(Option<PathBuf>),
    Healthcheck(Option<PathBuf>),
    Version,
    Help,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut command = None;
    let mut config = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--config" => {
                config = Some(PathBuf::from(args.next().ok_or("--config needs a path")?));
            }
            "serve" | "healthcheck" if command.is_none() => command = Some(arg),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(match command.as_deref() {
        Some("healthcheck") => Command::Healthcheck(config),
        _ => Command::Serve(config),
    })
}

fn main() -> ExitCode {
    let command = match parse_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let load = |path: Option<PathBuf>| Config::load(path.as_deref(), |k| std::env::var(k).ok());
    match command {
        Command::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("lodestar {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Healthcheck(path) => match load(path) {
            Ok(config) => match healthcheck(config.http.bind) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("unhealthy: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                eprintln!("config: {e}");
                ExitCode::FAILURE
            }
        },
        Command::Serve(path) => {
            let config = match load(path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("config: {e}");
                    return ExitCode::FAILURE;
                }
            };
            init_logging(&config.log);
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("starting the runtime: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match runtime.block_on(serve(config)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn init_logging(filter: &str) {
    let filter = tracing_subscriber::EnvFilter::try_new(filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn serve(config: Config) -> Result<(), BoxError> {
    std::fs::create_dir_all(&config.data_dir)
        .map_err(|e| format!("creating {}: {e}", config.data_dir.display()))?;
    let pipeline_config = config.pipeline_config()?;
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
    let pipeline = Pipeline::start(pipeline_config, store, plugins).await?;
    let state = http::AppState {
        monitor: pipeline.monitor(),
        dht: dht_stats,
        reader: Arc::new(Mutex::new(reader)),
        search: live.as_ref().map(|(index, l)| http::SearchState {
            index: index.clone(),
            stats: l.stats(),
        }),
    };
    let listener = tokio::net::TcpListener::bind(config.http.bind)
        .await
        .map_err(|e| format!("binding {}: {e}", config.http.bind))?;
    tracing::info!(
        "status page on http://{}/ (data in {})",
        config.http.bind,
        config.data_dir.display()
    );
    axum::serve(listener, http::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("shutting down: saving the queue and DHT state");
    pipeline.shutdown().await?;
    if let Some((_, live)) = live {
        // After the writer, so the last batch gets indexed too.
        tokio::task::spawn_blocking(move || live.stop()).await??;
    }
    tracing::info!("stopped cleanly");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

/// GET /healthz on the local instance with plain std networking (the
/// distroless image has no curl).
fn healthcheck(bind: SocketAddr) -> Result<(), String> {
    let mut addr = bind;
    if addr.ip().is_unspecified() {
        addr.set_ip(match addr.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        });
    }
    let timeout = Duration::from_secs(3);
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("connecting to {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    stream
        .take(4096)
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&response);
    let status = text.lines().next().unwrap_or("");
    if status.split_whitespace().nth(1) == Some("200") {
        Ok(())
    } else {
        Err(text.lines().last().unwrap_or(status).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Result<Command, String> {
        parse_args(s.split_whitespace().map(String::from))
    }

    #[test]
    fn parses_the_command_line() {
        assert_eq!(args(""), Ok(Command::Serve(None)));
        assert_eq!(
            args("serve --config /x.toml"),
            Ok(Command::Serve(Some("/x.toml".into())))
        );
        assert_eq!(
            args("--config c.toml healthcheck"),
            Ok(Command::Healthcheck(Some("c.toml".into())))
        );
        assert_eq!(args("--version"), Ok(Command::Version));
        assert_eq!(args("serve -h"), Ok(Command::Help));
        assert!(args("serve serve").is_err());
        assert!(args("--config").is_err());
        assert!(args("reindex").is_err());
    }

    #[test]
    fn healthcheck_needs_a_200() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let serve = |reply: &'static str| {
            let l = listener.try_clone().unwrap();
            std::thread::spawn(move || {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 256];
                let _ = s.read(&mut buf);
                s.write_all(reply.as_bytes()).unwrap();
            })
        };
        let t = serve("HTTP/1.1 200 OK\r\ncontent-length: 3\r\n\r\nok\n");
        assert!(healthcheck(SocketAddr::from(([0, 0, 0, 0], addr.port()))).is_ok());
        t.join().unwrap();
        let t = serve("HTTP/1.1 503 Service Unavailable\r\n\r\nstopped: dht\n");
        assert_eq!(healthcheck(addr), Err("stopped: dht".into()));
        t.join().unwrap();
        drop(listener);
        assert!(healthcheck(addr).is_err());
    }
}
