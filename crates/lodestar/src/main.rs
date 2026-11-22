//! The `lodestar` binary (architecture §11): `serve` (the default) runs the
//! crawler with its status page and metrics; `healthcheck` probes a
//! running instance for Docker's `HEALTHCHECK`. Arguments are parsed in
//! `cli`, which also runs the maintenance commands.

mod cli;
mod cluster;
mod companion_wiring;
mod companions;
mod config;
mod data_dir;
mod filter_load;
mod fixture;
mod floor;
mod fsdetect;
mod http;
mod insights;
mod logging;
mod memstats;
mod metrics;
mod net_watch;
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
mod text;
mod thp;
mod torrent_cache;
mod update;
mod upload;
mod watch;
mod web;
#[cfg(test)]
mod web_build;
mod windows;
mod worker;

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
    // A copy started by Restart now that fails waits for Enter on its
    // console, so its window doesn't close unread (X-win-restart-followups-2).
    // Read first, before anything runs: a bad config fails early.
    let relaunched = system::Restart::global().relaunched();
    system::restart::hold_if_relaunch_failed(relaunched, run())
}

fn run() -> ExitCode {
    // Started as the companion launcher (CMP-4b): apply the limits and
    // exec the companion before anything reads arguments, config or
    // logging. Returns only on failure. Linux only.
    #[cfg(target_os = "linux")]
    if let Some(code) = lodestar_companions::supervise::limits::launcher_main() {
        std::process::exit(code);
    }
    // Before any thread or arena growth; the `--child` inherits it (X-thp-off).
    thp::apply();
    if let Some(code) = windows::hook() {
        return code;
    }
    let cli = cli::parse();
    let config_path = cli.config;
    let command = cli.command.unwrap_or(Command::Serve {
        fixture: None,
        fixture_variant: None,
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
        // A stray `--fixture-variant` is refused below, before anything is
        // touched.
        Command::Serve {
            fixture: Some(_), ..
        }
        | Command::Serve {
            fixture_variant: Some(_),
            ..
        } => None,
        Command::Serve { fixture: None, .. }
        | Command::ImportBitmagnet(_)
        | Command::Import(_)
        | Command::Backup(_)
        | Command::Rollback(_)
        | Command::Vacuum
        | Command::Reindex
        | Command::Rollup(_)
        | Command::Worker(_)
        | Command::Companion(_)
        | Command::Settings(_) => Some(privdrop::Scope::DataDir),
        Command::Healthcheck | Command::Stats(_) => Some(privdrop::Scope::ProcessOnly),
        Command::Lists => None,
        // Windows only, where there's no PUID/PGID drop; it reads no config.
        Command::UpdateHelper(_) => None,
    };
    // `lodestar worker --data <dir>`: that folder is the one to own, through
    // the same checks as LODESTAR_DATA_DIR (it's passed to the drop as that
    // variable); `prepare` refuses what can't be checked the same way.
    let worker_data = match &command {
        Command::Worker(a) => match cli::worker::prepare(a) {
            Ok(d) => d,
            Err(code) => return code,
        },
        _ => None,
    };
    let env = |k: &str| match (k, &worker_data) {
        ("LODESTAR_DATA_DIR", Some(d)) => Some(d.clone()),
        _ => std::env::var(k).ok(),
    };
    let dropped = match scope.map(|s| privdrop::run(s, env)) {
        Some(Ok(report)) => report,
        Some(Err(e)) => {
            eprintln!("PUID/PGID: {e}");
            return ExitCode::FAILURE;
        }
        None => privdrop::Report::default(),
    };
    // Hand-run `serve` and `healthcheck`: the service's folder only once
    // it's found private, as for every other command (S8-07).
    let load = |path: Option<PathBuf>| {
        cli::load_serve_config(
            path.as_deref(),
            &cli::Install::here(),
            |k| std::env::var(k).ok(),
            cli::check_this_service_dir,
        )
    };
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
            Err(e) => cli::report(&e),
        },
        Command::Serve {
            fixture: Some(dir),
            fixture_variant,
            ..
        } => fixture::serve(dir, fixture_variant, config_path.as_deref()),
        // `--fixture-variant` needs `--fixture` (clap refuses it without),
        // and plain `serve` never takes one even if a parse let it through.
        Command::Serve {
            fixture: None,
            fixture_variant: Some(_),
            ..
        } => {
            eprintln!("lodestar serve: --fixture-variant needs --fixture");
            ExitCode::from(2)
        }
        Command::Serve {
            fixture: None,
            fixture_variant: None,
            child,
        } => {
            let config = match load(config_path.clone()) {
                Ok(c) => c,
                Err(e) => return cli::report(&e),
            };
            // The start-up checks the hook left here, once the service's
            // folder passed: run once either way.
            #[cfg(windows)]
            windows::STARTUP.run_deferred(|| windows::startup(&config));
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
            // `serve` finishes a portable rollback itself (below), so it
            // checks only the helper's and the supervisor's marker.
            if let Err(e) = cli::util::refuse_during_helper_rollback(&config.data_dir) {
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
            // The portable build's update (PK-win-portable-upd): any step
            // left to finish, before anything opens the store.
            #[cfg(windows)]
            let probation = match update::windows::portable::serve::before(&config) {
                Ok(p) => p,
                Err(code) => return code,
            };
            #[cfg(not(windows))]
            let probation: Probation = None;
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
            let served = runtime.block_on(serve_as(config, supervised, probation));
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

/// An unsupervised run: `serve --fixture`, its only caller, so release
/// builds without the `fixture` feature leave it out.
#[cfg(feature = "fixture")]
async fn serve(config: Config) -> Result<(), BoxError> {
    serve_as(config, None, None).await.map(|_| ())
}

/// `Some(code)` to exit with a reserved code for the supervisor.
/// The Windows portable build's new version on probation: `<data>\bin\.lock`,
/// held until its self-test decides.
#[cfg(windows)]
type Probation = Option<update::layout::Lock>;
#[cfg(not(windows))]
type Probation = Option<std::convert::Infallible>;

/// `probation`: the Windows portable build's new version, on probation.
async fn serve_as(
    config: Config,
    child: Supervised,
    probation: Probation,
) -> Result<Option<u8>, BoxError> {
    #[cfg(windows)]
    let mut probation = probation;
    let on_probation = probation.is_some();
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
    // `LODESTAR_EXIT_ON_NETWORK_LOSS`: the network this container shares
    // has gone (`net_watch`), so stop and exit for the restart policy.
    let network_lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let lost_flag = network_lost.clone();
    let watch = config.exit_on_network_loss;
    let stop = async move {
        tokio::select! {
            _ = signal.changed() => {}
            () = restart.requested() => {}
            () = net_watch::lost(watch) => {
                lost_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    };
    let services = match Services::start(&config).await {
        Ok(s) => s,
        Err(e) => {
            #[cfg(windows)]
            if let Some(code) = probation.take().and_then(|lock| {
                update::windows::portable::serve::start_failed(&config.data_dir, lock)
            }) {
                eprintln!("lodestar serve: {e}; the previous version is starting instead");
                return Ok(Some(code));
            }
            return Err(e);
        }
    };
    #[cfg(unix)]
    if let Some(c) = child {
        c.after_start(services.probe(), update::testing::limits().deadline);
    }
    #[cfg(windows)]
    let verdict = probation.take().map(|lock| {
        update::windows::portable::serve::after_start(
            config.data_dir.clone(),
            services.portable_probe(),
            lock,
        )
    });
    services.run(stop).await?;
    // A graceful stop before the self-test decided gives its attempt
    // back; the verdict and this never run at once (`portable::Verdict`).
    #[cfg(windows)]
    if let Some(v) = verdict {
        update::windows::portable::serve::stopped(config.data_dir.clone(), v).await;
    }
    let _ = (on_probation, probation);
    #[cfg(unix)]
    if let Some(c) = child {
        c.record_stored();
        if update::probation::failed() {
            return Err("the self-test failed".into());
        }
    }
    // A signal during the stop still wins: then it's a plain stop.
    if network_lost.load(std::sync::atomic::Ordering::SeqCst) && !*stopped.borrow_and_update() {
        return Ok(Some(net_watch::EXIT_CODE));
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
        error = lodestar_log::Error::kind(failure_kind(e, "failed"))
    );
    eprintln!(
        "{command}: {}",
        lodestar_log::url::scrub_text(&e.to_string())
    );
    ExitCode::FAILURE
}

/// A fixed label for why a command or the service failed, for the log
/// line, which gets no message because a message can name a path. The
/// store's refusal of a folder without WAL is named, as its fix is the
/// user's (X-store-wal-nits); anything else is `default`.
pub(crate) fn failure_kind(
    e: &(dyn std::error::Error + 'static),
    default: &'static str,
) -> &'static str {
    // Bounded, in case a chain loops back on itself.
    let wal = std::iter::successors(Some(e), |e| e.source())
        .take(16)
        .any(|e| {
            matches!(
                e.downcast_ref::<lodestar_store::Error>(),
                Some(lodestar_store::Error::WalUnavailable)
            )
        });
    if wal {
        "store_wal_unavailable"
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WAL refusal is named wherever it sits in the chain; any other
    /// failure keeps the caller's label.
    #[test]
    fn a_store_without_wal_is_named_in_the_failure_kind() {
        #[derive(Debug)]
        struct Wrapped(lodestar_store::Error);
        impl std::fmt::Display for Wrapped {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "opening the store")
            }
        }
        impl std::error::Error for Wrapped {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let bare: BoxError = Box::new(lodestar_store::Error::WalUnavailable);
        assert_eq!(failure_kind(&*bare, "failed"), "store_wal_unavailable");
        let wrapped: BoxError = Box::new(Wrapped(lodestar_store::Error::WalUnavailable));
        assert_eq!(failure_kind(&*wrapped, "serve"), "store_wal_unavailable");
        let other: BoxError = Box::new(lodestar_store::Error::Corrupt("x"));
        assert_eq!(failure_kind(&*other, "serve"), "serve");
        // What the console and `docker logs` show: the plain fix.
        assert!(
            bare.to_string().starts_with(
                "the data folder can't use SQLite's WAL journal: keep it on a local disk"
            ),
            "{bare}"
        );
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
