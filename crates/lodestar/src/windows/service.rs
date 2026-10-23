//! Lodestar as a Windows service (architecture §11).
//!
//! The installer (P-release) registers the service as [`NAME`] with the
//! image path `"<install dir>\lodestar.exe"`, optionally followed by
//! `--config <path>`. No subcommand marks a service start: when the
//! arguments are a serve command line and the process has no console,
//! `main` asks the Service Control Manager whether it started it
//! ([`run_if_launched`]), and an ordinary run is told no at once.
//!
//! Started by the SCM, Lodestar:
//! - first secures `%ProgramData%\Lodestar` ([`super::acl`]), and refuses
//!   to start (service-specific exit code 3) if it can't;
//! - keeps its data there unless `LODESTAR_DATA_DIR` or the config file
//!   says otherwise, and runs from that folder, so relative paths in the
//!   config land there;
//! - logs to `<data>\logs\lodestar.log` ([`super::logfile`]), where the
//!   first run's setup token appears;
//! - maps Stop, Shutdown and Preshutdown to the same graceful shutdown as
//!   Ctrl+C, acknowledging at once (also while the store opens) and
//!   reporting progress so the SCM waits for it;
//! - reports a service-specific exit code 1 when it stops on an error, so
//!   the SCM's recovery actions (restart) apply.

use std::path::{Path, PathBuf};

use crate::config::Config;

/// The service's name, for the installer and `sc.exe`.
pub const NAME: &str = "Lodestar";

/// The data folder when nothing else sets one: `%ProgramData%\Lodestar`.
pub fn default_data_dir(program_data: Option<String>) -> PathBuf {
    PathBuf::from(program_data.unwrap_or_else(|| r"C:\ProgramData".into())).join("Lodestar")
}

/// The config file: `path` if given, else `config.toml` in
/// `LODESTAR_DATA_DIR` or the default data folder, if it exists.
pub fn config_path(
    path: Option<PathBuf>,
    default_dir: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    path.or_else(|| {
        let dir = env("LODESTAR_DATA_DIR").map_or_else(|| default_dir.to_path_buf(), PathBuf::from);
        let p = dir.join("config.toml");
        p.exists().then_some(p)
    })
}

/// Puts the data in `default_dir` unless the config file or the
/// environment chose a folder.
pub fn default_data_dir_unless_set(config: &mut Config, default_dir: &Path) {
    if !config.layers.in_file("data_dir") && !config.layers.in_env("LODESTAR_DATA_DIR") {
        config.data_dir = default_dir.to_path_buf();
    }
}

/// The config path when the arguments are a service command line (none,
/// `serve`, and an optional `--config <path>`), else `None`: every other
/// command runs as usual, without asking the SCM anything.
pub fn service_command<I, T>(args: I) -> Option<Option<PathBuf>>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::Parser;
    let cli = crate::cli::Cli::try_parse_from(args).ok()?;
    matches!(cli.command, None | Some(crate::cli::Command::Serve)).then_some(cli.config)
}

#[cfg(windows)]
pub use scm::run_if_launched;

#[cfg(windows)]
mod scm {
    use std::ffi::OsString;
    use std::io::IsTerminal;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use tokio::sync::watch;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };
    use windows_service::{define_windows_service, service_dispatcher};

    use super::NAME;
    use crate::config::Config;
    use crate::services::{BoxError, Services};
    use crate::windows::acl;
    use crate::windows::logfile::{self, LogFile};

    /// How long the SCM should wait between progress reports while
    /// stopping; progress is reported every [`STOP_TICK`].
    const STOP_WAIT_HINT: Duration = Duration::from_secs(30);
    const STOP_TICK: Duration = Duration::from_secs(5);
    /// The exit code for a start refused for safety (the CLI's 3).
    const REFUSED: u32 = 3;

    /// The `--config` path from the command line, for `service_main`.
    static CONFIG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    /// The service's exit code, for the process's.
    static EXIT: OnceLock<u32> = OnceLock::new();
    /// For the control handler, which runs on the dispatcher's thread.
    static STATUS: OnceLock<ServiceStatusHandle> = OnceLock::new();
    static CHECKPOINT: AtomicU32 = AtomicU32::new(1);

    define_windows_service!(ffi_service_main, service_main);

    /// Runs Lodestar as a service if the SCM started this process with
    /// `config` from its command line, and returns its exit code once it
    /// has stopped; `None` for an ordinary run.
    pub fn run_if_launched(config: Option<PathBuf>) -> Option<ExitCode> {
        // A service has no console; a console run needn't ask the SCM.
        if std::io::stdin().is_terminal() {
            return None;
        }
        let _ = CONFIG_PATH.set(config);
        // Fails at once (ERROR_FAILED_SERVICE_CONTROLLER_CONNECT) unless
        // the SCM started this process.
        service_dispatcher::start(NAME, ffi_service_main).ok()?;
        Some(match EXIT.get() {
            Some(0) => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        })
    }

    fn service_main(args: Vec<OsString>) {
        // The SCM passes the service's registered name first.
        let name = args
            .first()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or(NAME)
            .to_string();
        let _ = EXIT.set(run(&name));
    }

    /// The service's life: register, run, report the end.
    fn run(name: &str) -> u32 {
        let (stop_tx, stop_rx) = watch::channel(false);
        let handler = move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                // Acknowledged at once, even while the store is opening.
                if let Some(status) = STATUS.get() {
                    stop_pending(*status);
                }
                let _ = stop_tx.send(true);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(status) = service_control_handler::register(name, handler) else {
            return 1;
        };
        let _ = STATUS.set(status);
        // Running at once: opening a large store can take longer than
        // the SCM waits for a start, and /healthz says when it's ready.
        report(
            status,
            ServiceState::Running,
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::PRESHUTDOWN,
            0,
        );
        let default_dir = super::default_data_dir(std::env::var("ProgramData").ok());
        // Before anything in it is read or written. A refusal can't be
        // logged in the folder; the exit code (3) says it was refused.
        let code = if acl::secure_dir(&default_dir, |d, l| acl::icacls(d, l, Some(name))).is_err() {
            REFUSED
        } else {
            match serve(&default_dir, status, stop_rx) {
                Ok(()) => {
                    tracing::info!("service: stopped");
                    0
                }
                Err(e) => {
                    tracing::error!("service: stopped on an error: {e}");
                    1
                }
            }
        };
        report(
            status,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            code,
        );
        code
    }

    fn report(
        status: ServiceStatusHandle,
        state: ServiceState,
        accept: ServiceControlAccept,
        code: u32,
    ) {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: if code == 0 {
                ServiceExitCode::NO_ERROR
            } else {
                ServiceExitCode::ServiceSpecific(code)
            },
            checkpoint: 0,
            wait_hint: Duration::ZERO,
            process_id: None,
        });
    }

    /// Reports StopPending with the next checkpoint.
    fn stop_pending(status: ServiceStatusHandle) {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::StopPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::NO_ERROR,
            checkpoint: CHECKPOINT.fetch_add(1, Ordering::Relaxed),
            wait_hint: STOP_WAIT_HINT,
            process_id: None,
        });
    }

    fn serve(
        default_dir: &Path,
        status: ServiceStatusHandle,
        stop: watch::Receiver<bool>,
    ) -> Result<(), BoxError> {
        let default_dir = default_dir.to_path_buf();
        let _ = std::env::set_current_dir(&default_dir);
        let env = |k: &str| std::env::var(k).ok();
        let path = CONFIG_PATH.get().cloned().flatten();
        let path = super::config_path(path, &default_dir, env);
        let config = Config::load(path.as_deref(), env)
            .map(|mut c| {
                super::default_data_dir_unless_set(&mut c, &default_dir);
                c
            })
            .map_err(|e| format!("config: {e}"));
        let data_dir = config.as_ref().map_or(default_dir, |c| c.data_dir.clone());
        init_logging(
            &data_dir,
            config.as_ref().map_or("info", |c| c.log.as_str()),
        );
        tracing::info!("service: starting (data in {})", data_dir.display());
        let config = config?;
        super::super::startup(&config);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let mut early = stop.clone();
            // A stop while the store opens ends the start: the open's
            // blocking work finishes as the runtime drops, and SQLite
            // needs nothing more.
            let services = tokio::select! {
                s = Services::start(&config) => s?,
                () = stopped(&mut early, status) => return Ok(()),
            };
            let mut stop = stop;
            services
                .run(async move { stopped(&mut stop, status).await })
                .await
        })
    }

    /// Resolves once a stop is requested, then keeps reporting progress
    /// to the SCM until the runtime ends.
    async fn stopped(stop: &mut watch::Receiver<bool>, status: ServiceStatusHandle) {
        while !*stop.borrow_and_update() {
            if stop.changed().await.is_err() {
                // The handler is gone; nothing can stop us later.
                std::future::pending::<()>().await;
            }
        }
        tracing::info!("service: stopping");
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(STOP_TICK).await;
                stop_pending(status);
            }
        });
    }

    /// The service's subscriber: the log file, no console.
    ///
    /// A log level saved in Settings applies at the next start here: the
    /// live reload handle is private to `settings::log`, whose `init`
    /// writes to stderr only.
    fn init_logging(data_dir: &std::path::Path, filter: &str) {
        let Ok(file) = LogFile::open(logfile::path(data_dir), logfile::MAX_BYTES) else {
            return;
        };
        let filter = crate::settings::log::parse(filter)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(file),
            )
            .try_init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(s: &str) -> Option<Option<PathBuf>> {
        service_command(std::iter::once("lodestar").chain(s.split_whitespace()))
    }

    #[test]
    fn only_a_serve_command_line_is_a_service_start() {
        assert_eq!(command(""), Some(None));
        assert_eq!(command("serve"), Some(None));
        assert_eq!(command("--config c.toml"), Some(Some("c.toml".into())));
        assert_eq!(
            command("serve --config c.toml"),
            Some(Some("c.toml".into()))
        );
        assert_eq!(
            command("--config c.toml serve"),
            Some(Some("c.toml".into()))
        );
        for other in [
            "healthcheck",
            "backup",
            "stats --json",
            "lists",
            "--config",
            "--help",
            "bogus",
        ] {
            assert_eq!(command(other), None, "{other}");
        }
    }

    #[test]
    fn the_default_data_folder_yields_to_the_file_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let default = dir.path().join("pd");
        let none = |_: &str| None;
        // Nothing set: the default, and no config file there yet.
        assert_eq!(config_path(None, &default, none), None);
        let mut c = Config::load(None, none).unwrap();
        default_data_dir_unless_set(&mut c, &default);
        assert_eq!(c.data_dir, default);
        // A config.toml in the default folder is found.
        std::fs::create_dir_all(&default).unwrap();
        std::fs::write(default.join("config.toml"), "data_dir = 'elsewhere'\n").unwrap();
        let path = config_path(None, &default, none);
        assert_eq!(path, Some(default.join("config.toml")));
        // The file's data_dir wins over the default.
        let mut c = Config::load(path.as_deref(), none).unwrap();
        default_data_dir_unless_set(&mut c, &default);
        assert_eq!(c.data_dir, PathBuf::from("elsewhere"));
        // So does LODESTAR_DATA_DIR, which also says where the file is.
        let other = dir.path().join("env");
        let env = |k: &str| (k == "LODESTAR_DATA_DIR").then(|| other.display().to_string());
        assert_eq!(config_path(None, &default, env), None);
        let mut c = Config::load(None, env).unwrap();
        default_data_dir_unless_set(&mut c, &default);
        assert_eq!(c.data_dir, other);
        // An explicit --config is taken as given.
        assert_eq!(
            config_path(Some("x.toml".into()), &default, none),
            Some("x.toml".into())
        );
    }

    #[test]
    fn data_lives_in_program_data() {
        assert_eq!(
            default_data_dir(Some(r"D:\PD".into())),
            PathBuf::from(r"D:\PD").join("Lodestar")
        );
        assert_eq!(
            default_data_dir(None),
            PathBuf::from(r"C:\ProgramData").join("Lodestar")
        );
    }
}
