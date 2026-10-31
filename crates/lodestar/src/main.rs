//! The `lodestar` binary (architecture §11): `serve` (the default) runs the
//! crawler with its status page and metrics; `healthcheck` probes a
//! running instance for Docker's `HEALTHCHECK`. Arguments are parsed in
//! `cli`, which also runs the maintenance commands.

mod cli;
mod config;
mod filter_load;
mod fixture;
mod floor;
mod fsdetect;
mod http;
mod insights;
mod logging;
mod metrics;
mod ops;
mod percolator;
mod privdrop;
mod services;
mod settings;
mod storage;
mod subscriptions;
mod system;
#[cfg(test)]
mod test_log;
mod update;
mod web;
#[cfg(test)]
mod web_build;
mod windows;

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::cli::Command;
use crate::config::Config;
use crate::services::{BoxError, Services};

// musl's allocator is a severe bottleneck for multithreaded Rust (§11).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    if let Some(code) = windows::hook() {
        return code;
    }
    let cli = cli::parse();
    let config_path = cli.config;
    let command = cli.command.unwrap_or(Command::Serve {
        fixture: None,
        child: false,
    });
    // Root is dropped first: before the config file (in the data directory)
    // is read, any socket is bound or any thread is spawned. Every command
    // that touches the data directory runs as the target account, so a
    // backup, rollback or vacuum never leaves root-owned files there; none
    // needs root. `stats` and `healthcheck` only read. `lists` prints a
    // built-in table and touches nothing, so it runs as started.
    let scope = match command {
        // `fixture::serve` drops it for the fixture's own folder, once it
        // has checked that folder; the data directory is never touched.
        Command::Serve {
            fixture: Some(_), ..
        } => None,
        Command::Serve { fixture: None, .. }
        | Command::ImportBitmagnet(_)
        | Command::Import(_)
        | Command::Backup(_)
        | Command::Rollback(_)
        | Command::Vacuum
        | Command::Reindex
        | Command::Rollup(_) => Some(privdrop::Scope::DataDir),
        Command::Healthcheck | Command::Stats(_) => Some(privdrop::Scope::ProcessOnly),
        Command::Lists => None,
    };
    let dropped = match scope.map(|s| privdrop::run(s, |k| std::env::var(k).ok())) {
        Some(Ok(report)) => report,
        Some(Err(e)) => {
            eprintln!("PUID/PGID: {e}");
            return ExitCode::FAILURE;
        }
        None => privdrop::Report::default(),
    };
    let load = |path: Option<PathBuf>| Config::load(path.as_deref(), |k| std::env::var(k).ok());
    match command {
        Command::Lists => {
            for e in subscriptions::CATALOG {
                println!(
                    "{}\n  {} ({})\n  {}\n  {}\n",
                    e.key,
                    e.name,
                    e.format.as_str(),
                    e.url,
                    e.about
                );
            }
            ExitCode::SUCCESS
        }
        Command::Healthcheck => match load(config_path) {
            Ok(config) => match healthcheck(system::address::health_bind(&config)) {
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
        Command::Serve {
            fixture: Some(dir), ..
        } => fixture::serve(dir, config_path.as_deref()),
        Command::Serve {
            fixture: None,
            child,
        } => {
            let config = match load(config_path.clone()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("config: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // On Unix, `serve` is the supervisor (§12.2), still
            // single-threaded here; the work runs in its `--child`.
            #[cfg(unix)]
            if !child {
                return update::supervisor::run(
                    &config.data_dir,
                    config.db_path(),
                    config.data_dir.join("index"),
                    config_path.as_deref(),
                    &dropped,
                );
            }
            #[cfg(not(unix))]
            if child {
                eprintln!("lodestar serve: --child is the Unix supervisor's own");
                return ExitCode::from(2);
            }
            // Every store opener refuses while a rollback journal exists.
            if let Err(e) = cli::util::refuse_during_update_rollback(&config.data_dir) {
                eprintln!("lodestar serve: {e}");
                return ExitCode::from(3);
            }
            // Logging first, so the child's update events are kept.
            init_logging(&config.log);
            dropped.log(&config.data_dir);
            #[cfg(unix)]
            let supervised =
                match update::probation::before_start(&config.data_dir, &config.db_path()) {
                    Ok(c) => {
                        if c.supervised {
                            system::Restart::global().set_supervised();
                        }
                        Some(c)
                    }
                    Err(e) => {
                        eprintln!("lodestar serve: {e}");
                        return ExitCode::FAILURE;
                    }
                };
            #[cfg(not(unix))]
            let supervised = None;
            fsdetect::warn_at_start(&config.data_dir);
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
            let served = runtime.block_on(serve_as(config, supervised));
            // A blocking task still running (a stuck read, a download's
            // reader) can't hold the exit.
            runtime.shutdown_timeout(services::RUNTIME_GRACE);
            match served {
                Ok(Some(code)) => ExitCode::from(code),
                Ok(None) => ExitCode::SUCCESS,
                Err(e) => command_failed("serve", &*e),
            }
        }
        other => cli::run(other, config_path.as_deref(), &dropped),
    }
}

fn init_logging(filter: &str) {
    // Reloadable: a saved `log_level` applies while serving.
    settings::log::init(filter);
}

#[cfg(unix)]
type Supervised = Option<&'static update::probation::Child>;
#[cfg(not(unix))]
type Supervised = Option<std::convert::Infallible>;

/// An unsupervised run (`serve --fixture`, Windows).
async fn serve(config: Config) -> Result<(), BoxError> {
    serve_as(config, None).await.map(|_| ())
}

/// `Some(code)` to exit with a reserved code for the supervisor.
async fn serve_as(config: Config, child: Supervised) -> Result<Option<u8>, BoxError> {
    // Stop on a signal, or restart when asked (`POST /system/restart`):
    // both take the same graceful shutdown. A signal wins, even during a
    // restart's shutdown.
    let restart = system::Restart::global();
    let (signalled, mut stopped) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = signalled.send(true);
    });
    let mut signal = stopped.clone();
    let stop = async move {
        tokio::select! {
            _ = signal.changed() => {}
            () = restart.requested() => {}
        }
    };
    let services = Services::start(&config).await?;
    #[cfg(unix)]
    if let Some(c) = child {
        c.after_start(services.probe(), update::testing::limits().deadline);
    }
    services.run(stop).await?;
    #[cfg(unix)]
    if let Some(c) = child {
        c.record_stored();
        if update::probation::failed() {
            return Err("the self-test failed".into());
        }
    }
    if restart.was_requested() && !*stopped.borrow_and_update() {
        // Under the supervisor: the request, then its reserved code.
        #[cfg(unix)]
        if let Some(c) = child.filter(|c| c.supervised) {
            let (action, version, lost) =
                update::control::intent().unwrap_or((update::control::Action::Restart, None, None));
            let run_id = c.hint.as_ref().map_or("", |h| h.run_id.as_str());
            let code = update::control::request(&c.bin, run_id, action, version, lost)?;
            return Ok(Some(code as u8));
        }
        let _ = child;
        // Returns only if the exec failed: exit non-zero, so a restart
        // policy takes over.
        return Err(format!("restarting: {}", system::restart::reexec()).into());
    }
    Ok(None)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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

/// A command's error: logged by its kind, and its text, scrubbed of URL
/// credentials (an import's database URL can hold a password), to stderr.
fn command_failed(command: &'static str, e: &(dyn std::error::Error + 'static)) -> ExitCode {
    lodestar_log::log_event!(
        logging::events::APP_COMMAND_FAILED,
        command = command,
        error = lodestar_log::Error::kind("failed")
    );
    eprintln!(
        "{command}: {}",
        lodestar_log::url::scrub_text(&e.to_string())
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

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
