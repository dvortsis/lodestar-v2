//! The command line (architecture §11), on clap. `serve` stays the
//! default, so a bare `lodestar` (Docker, Unraid, systemd) runs the server
//! as before, and the `LODESTAR_*` variables apply to every command.
//!
//! The maintenance commands live here:
//! - `backup`: a consistent copy, safe while the server runs;
//! - `rollback`: restore a pre-migration snapshot, server stopped;
//! - `vacuum`: compact the store, server stopped;
//! - `reindex`: rebuild the search index at the next start;
//! - `rollup --repair`: recount the Insights rollups from the store,
//!   server stopped;
//! - `stats`: counts, sizes and the index's state, read-only;
//! - `import`: bring a Magnetico or Bitmagnet database in (IMP-4), with
//!   `import-bitmagnet` kept as its old name;
//! - `companion`: list, install, remove or print a compose service for
//!   a companion (CMP-5);
//! - `settings`: get, set or reset the settings saved in the app, server
//!   stopped, through the same checks as the API ([`settings`], §10.10.6).
//!
//! Each refuses while B-update's rollback journal exists (§12.2), and the
//! offline ones refuse while anything has the store open ([`lock`]).

mod backup;
pub(crate) mod companion;
mod import;
mod lock;
mod reindex;
mod rollback;
mod rollup;
mod settings;
mod stats;
mod update_helper;
pub(crate) mod util;
mod vacuum;
pub(crate) mod worker;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::config::Config;
use crate::privdrop;
use util::Failure;

const AFTER_HELP: &str = "\
Environment: LODESTAR_DATA_DIR, LODESTAR_HTTP_BIND, LODESTAR_DHT_PORT,
LODESTAR_DHT_ENABLED, LODESTAR_INTENSITY, LODESTAR_SCRAPE_ENABLED, LODESTAR_LOG,
LODESTAR_SECURE_COOKIES

Exit status: 0 done, 1 failed, 2 bad arguments, 3 refused for safety
(nothing was changed).";

/// Lodestar: a DHT crawler and torrent-metadata search engine.
#[derive(Debug, Parser, PartialEq, Eq)]
#[command(
    name = "lodestar",
    version,
    after_help = AFTER_HELP,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// config.toml (default: $LODESTAR_DATA_DIR/config.toml or
    /// data/config.toml, if present)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Run the crawler, search, UI, status page and metrics (the default)
    Serve {
        /// Serve test data instead, offline: built in DIR if it's empty or
        /// new, reused if it holds a fixture (a temporary folder without
        /// DIR); any other folder is refused
        #[arg(long, value_name = "DIR", num_args = 0..=1)]
        fixture: Option<Option<PathBuf>>,
        /// Run as the supervisor's child (set by the supervisor, §12.2)
        #[arg(long, hide = true, conflicts_with = "fixture")]
        child: bool,
    },
    /// Exit 0 if the local instance answers /healthz, 1 otherwise
    Healthcheck,
    /// Import a Magnetico or Bitmagnet database: inspect, start, status,
    /// pause, resume, cancel, remove (see `lodestar import --help`)
    Import(import::ImportArgs),
    /// The old name for `lodestar import start --source bitmagnet`
    ImportBitmagnet(ImportBitmagnetArgs),
    /// Show the blocklist catalog ([subscriptions] catalog keys)
    Lists,
    /// Back up the store, config.toml, seal.key and auth.json; safe while
    /// the server runs
    Backup(BackupArgs),
    /// List the snapshots taken before each store upgrade, or restore one
    /// (server stopped)
    Rollback(RollbackArgs),
    /// Compact the store and report its size before and after (server
    /// stopped)
    Vacuum,
    /// Rebuild the search index from the store at the next start (server
    /// stopped)
    Reindex,
    /// Show counts, sizes and the index's state; safe while the server
    /// runs
    Stats(StatsArgs),
    /// Recount the Insights rollups from the stored torrents (server
    /// stopped)
    ///
    /// Rebuilds, for the window: stored and searchable counts per discovery
    /// cohort, newly stored and newly searchable per hour, the
    /// time-to-searchable histogram, and the imported total. It only raises
    /// totals. Lodestar runs it by itself over the 72 hours before the
    /// last activity when it starts after an unclean stop.
    ///
    /// Lost for good: counts from before storage (keys harvested, keys
    /// reported and rediscovered, peers found, metadata fetched, losses,
    /// drops), coverage and the hourly
    /// searchable snapshots, and torrents indexed before searchable time
    /// was tracked. Hourly detail older than 30 days is folded into days,
    /// so those days are repaired at daily grain.
    Rollup(RollupArgs),
    /// Run as a remote worker: join a coordinator with a one-time token
    /// (--join, --token-file), then run from the saved credentials
    Worker(worker::WorkerArgs),
    /// Companions: list, install, remove (server stopped), or print a
    /// compose service (see `lodestar companion --help`)
    Companion(companion::CompanionArgs),
    /// The settings saved in the app: get, set or reset them with the
    /// server stopped, checked as the app checks them (see `lodestar
    /// settings --help`)
    ///
    /// Recovery when the app can't be reached. `reset --all` removes every
    /// saved value; config.toml and the LODESTAR_* variables are never
    /// changed, and a variable always wins. `set` refuses secrets, privacy
    /// opt-ins and turning on adult content: change those in the app.
    Settings(settings::SettingsArgs),
    /// The MSI's update helper, run as SYSTEM by its scheduled task
    /// (Windows; §12.2, plan Q13)
    #[command(hide = true)]
    UpdateHelper(update_helper::UpdateHelperArgs),
}

/// Where the bitmagnet database is. The password comes from
/// --password-file or LODESTAR_IMPORT_PG_PASSWORD, never the command line.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct ImportBitmagnetArgs {
    /// PostgreSQL's host name or IP address, or its Unix socket folder
    #[arg(long, default_value = "localhost")]
    pub host: String,
    #[arg(long, default_value_t = 5432)]
    pub port: u16,
    #[arg(long, default_value = "bitmagnet")]
    pub dbname: String,
    #[arg(long, default_value = "bitmagnet_reader")]
    pub user: String,
    /// Read the password from this file (chmod 600); otherwise from
    /// LODESTAR_IMPORT_PG_PASSWORD
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// Trust only the CA certificates in this PEM file for TLS (default:
    /// the system's)
    #[arg(long, value_name = "PATH")]
    pub ca_file: Option<PathBuf>,
    /// Connect without TLS: allowed only to a Unix socket or a loopback
    /// address
    #[arg(long, conflicts_with = "ca_file")]
    pub allow_plaintext_loopback: bool,
    /// Stop after this many torrents
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct BackupArgs {
    /// The folder to put the backup in (default: <data>/backups); each
    /// backup is a new lodestar-backup-<time> folder inside it
    #[arg(long, value_name = "PATH")]
    pub to: Option<PathBuf>,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct RollbackArgs {
    /// The snapshot to restore: its file name, or vN; without it, list
    /// them
    pub snapshot: Option<String>,
    /// Restore without asking
    #[arg(long)]
    pub yes: bool,
    /// Roll a self-installed update back to the version before it
    /// (server stopped), through the same journal the supervisor uses;
    /// asks first unless --yes
    #[arg(long, conflicts_with = "snapshot")]
    pub update: bool,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct RollupArgs {
    /// Recount and raise what the store can rebuild (required)
    #[arg(long, required = true)]
    pub repair: bool,
    /// How far back, in days, like 90d (default: 30d, the hourly part);
    /// older days are repaired at daily grain
    #[arg(long, value_name = "Nd", value_parser = rollup::parse_days)]
    pub since: Option<u32>,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct StatsArgs {
    /// Print JSON
    #[arg(long)]
    pub json: bool,
}

/// Where this executable is, for [`msi_data_dir`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Install {
    /// This executable (`current_exe`).
    pub exe: Option<PathBuf>,
    /// `%ProgramFiles%`, as the system has it.
    pub program_files: Option<PathBuf>,
    /// The service's data folder, `%ProgramData%\Lodestar`: on Windows
    /// always known ([`Install::windows`]), elsewhere `None`.
    pub service_dir: Option<PathBuf>,
}

impl Install {
    /// This process's: on Windows only. Elsewhere nothing, so no
    /// executable is ever the MSI's and there's no service folder.
    pub(crate) fn here() -> Install {
        #[cfg(windows)]
        {
            Install::windows(
                std::env::current_exe().ok(),
                crate::update::windows::msi::system_program_files(),
                std::env::var("ProgramData").ok(),
            )
        }
        #[cfg(not(windows))]
        {
            Install::default()
        }
    }

    /// A Windows install. The service folder is worked out exactly as the
    /// service works out its own (`windows::service::default_data_dir`),
    /// with the same fallback when `%ProgramData%` is unset, so it's never
    /// unknown on Windows.
    // Off Windows only the tests call it.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn windows(
        exe: Option<PathBuf>,
        program_files: Option<PathBuf>,
        program_data: Option<String>,
    ) -> Install {
        Install {
            exe,
            program_files,
            service_dir: Some(crate::windows::service::default_data_dir(program_data)),
        }
    }
}

/// The MSI's data folder, `%ProgramData%\Lodestar`, when this is the MSI's
/// executable: `%ProgramFiles%\Lodestar\lodestar.exe`, the only place the
/// MSI installs (`main.wxs`). It's where the service keeps its data when
/// nothing else chose a folder, so the CLI finds the same store. `None`
/// for any other executable (the portable zip, Docker, Linux, a build),
/// which keeps `data` in the working folder as before.
pub(crate) fn msi_data_dir(install: &Install) -> Option<PathBuf> {
    let (exe, pf) = (install.exe.as_deref()?, install.program_files.as_deref()?);
    let msi = crate::windows::service::same_dir(exe, &pf.join("Lodestar").join("lodestar.exe"));
    install.service_dir.clone().filter(|_| msi)
}

/// Why the service's folder can't be used, from [`check_service_dir`].
pub(crate) type FolderCheck = Result<(), Failure>;

/// Whether the CLI may use the service's folder `dir`, judged as the
/// service judges it before using it ([`acl::secure_dir`]'s read-only
/// half: [`acl::Ops::inspect`] and [`acl::verdict`]). It never creates
/// the folder and changes nothing:
/// - private: `Ok`;
/// - not private: refused, with the service's own words
///   ([`acl::refusal`]);
/// - its permissions can't be read: refused, with a hint to run the
///   command as an administrator;
/// - missing: no store there, and nothing is created.
///
/// [`acl::secure_dir`]: crate::windows::acl::secure_dir
/// [`acl::Ops::inspect`]: crate::windows::acl::Ops::inspect
/// [`acl::verdict`]: crate::windows::acl::verdict
/// [`acl::refusal`]: crate::windows::acl::refusal
// Off Windows only the tests call it.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn check_service_dir(
    dir: &Path,
    own_sid: Option<&str>,
    ops: &dyn crate::windows::acl::Ops,
) -> FolderCheck {
    use crate::windows::acl;
    let seen = match ops.inspect(dir) {
        Ok(Some(seen)) => seen,
        Ok(None) => return Err(Failure::Failed(util::no_store(&dir.join("lodestar.db")))),
        Err(e) => {
            return Err(Failure::Refused(format!(
                "{} is Lodestar's service folder, and its permissions couldn't be read ({e}), \
                 so Lodestar won't use it. Run the command from PowerShell opened with Run as \
                 administrator.",
                dir.display()
            )));
        }
    };
    acl::verdict(&seen, own_sid).map_err(|why| {
        Failure::Refused(acl::refusal(dir, &seen, own_sid, &why, &|sid| {
            ops.account(sid)
        }))
    })
}

/// [`check_service_dir`] for this machine's service: on Windows, with the
/// service's SID. Elsewhere it's never asked ([`Install::here`] names no
/// executable there), and refuses if it were.
pub(crate) fn check_this_service_dir(dir: &Path) -> FolderCheck {
    #[cfg(windows)]
    {
        use crate::windows::{acl, service};
        let own = acl::service_sid(service::NAME).ok();
        check_service_dir(
            dir,
            own.as_deref(),
            &acl::Windows {
                own_sid: own.as_deref(),
            },
        )
    }
    #[cfg(not(windows))]
    {
        Err(Failure::Refused(format!(
            "{} is a Windows service's folder",
            dir.display()
        )))
    }
}

/// Why [`load_config`] found no config.
#[derive(Debug)]
pub(crate) enum LoadError {
    Config(crate::config::ConfigError),
    /// The service's folder can't be used ([`check_service_dir`]).
    Folder(Failure),
}

impl From<crate::config::ConfigError> for LoadError {
    fn from(e: crate::config::ConfigError) -> LoadError {
        LoadError::Config(e)
    }
}

/// [`check_service_dir`] for the service's folder, at most once,
/// whenever that folder is used: the one rule that [`load_config`],
/// [`load_serve_config`] and so every hand-run command share.
pub(crate) struct ServiceGuard<C> {
    /// The service's folder; `None` off Windows, so nothing is checked.
    dir: Option<PathBuf>,
    /// Taken by the first check.
    check: Option<C>,
}

impl<C: FnOnce(&Path) -> FolderCheck> ServiceGuard<C> {
    pub(crate) fn new(install: &Install, check: C) -> ServiceGuard<C> {
        ServiceGuard {
            dir: install.service_dir.clone(),
            check: Some(check),
        }
    }

    /// Whether `p` is the service's folder, compared as Windows compares
    /// paths. An empty path is the working folder, as it is when used.
    fn is_service(&self, p: &Path) -> bool {
        let p = if p.as_os_str().is_empty() {
            Path::new(".")
        } else {
            p
        };
        self.dir
            .as_deref()
            .is_some_and(|s| crate::windows::service::same_dir(p, s))
    }

    fn ensure(&mut self) -> Result<(), LoadError> {
        match (self.dir.as_deref(), self.check.take()) {
            (Some(dir), Some(c)) => c(dir).map_err(LoadError::Folder),
            _ => Ok(()),
        }
    }

    /// Before anything is read: when the data folder named by
    /// `LODESTAR_DATA_DIR` (or `worker --data`) is the service's, when the
    /// config file is inside it, or when the caller reads `config.toml`
    /// there (`reads_there`).
    pub(crate) fn before(
        &mut self,
        config_path: Option<&Path>,
        data_dir_env: Option<&str>,
        reads_there: bool,
    ) -> Result<(), LoadError> {
        let Some(svc) = self.dir.clone() else {
            return Ok(());
        };
        let named = data_dir_env.is_some_and(|d| self.is_service(Path::new(d)));
        let inside = config_path.is_some_and(|p| crate::windows::service::within(p, &svc));
        if named || inside || reads_there {
            self.ensure()?;
        }
        Ok(())
    }

    /// After loading, before anything opens the store: when the config's
    /// data folder is the service's.
    pub(crate) fn after(&mut self, data_dir: &Path) -> Result<(), LoadError> {
        if self.is_service(data_dir) {
            self.ensure()?;
        }
        Ok(())
    }
}

/// The config for hand-run `serve` and `healthcheck`: read as before
/// (`--config`, `LODESTAR_DATA_DIR`, `data`), never defaulting to the
/// service's folder, but with [`ServiceGuard`]'s check whenever that
/// folder is used. A service the SCM starts never comes here: it checks
/// its own folder (`windows::service`).
pub(crate) fn load_serve_config(
    config_path: Option<&Path>,
    install: &Install,
    env: impl Fn(&str) -> Option<String>,
    check: impl FnOnce(&Path) -> FolderCheck,
) -> Result<Config, LoadError> {
    load_config(&Command::Healthcheck, config_path, install, env, check).map(|(c, _)| c)
}

/// Prints why the config couldn't be had, and its exit status: 3 for a
/// refusal (nothing was changed), 1 otherwise.
pub(crate) fn report(e: &LoadError) -> ExitCode {
    match e {
        LoadError::Config(e) => {
            eprintln!("config: {e}");
            ExitCode::FAILURE
        }
        LoadError::Folder(e @ Failure::Refused(_)) => {
            eprintln!("lodestar: {e}");
            ExitCode::from(3)
        }
        LoadError::Folder(e @ Failure::Failed(_)) => {
            eprintln!("lodestar: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The config for `command`, and the `config.toml` it was read from (if
/// any). `lodestar worker --data <dir>` is the data folder for the config
/// too (its config.toml, its log), as it is for the privilege drop, so the
/// two agree.
///
/// Run from the MSI's executable ([`msi_data_dir`]) with no
/// `LODESTAR_DATA_DIR`, the folder is the service's, found as the service
/// finds it: `config.toml` there if present, and the data there unless the
/// config file chose another folder. `--config` and `LODESTAR_DATA_DIR`
/// still win. `worker` never defaults to it: a worker's folder is the
/// installer's to set up, and `--data` stays explicit. Nor do `serve` and
/// `healthcheck` ([`load_serve_config`]).
///
/// Whenever the service's folder (`%ProgramData%\Lodestar`) is used, from
/// any executable and however it was named, `check` (on Windows,
/// [`check_service_dir`]) must find it private first, as the service does
/// before using it:
/// - before anything is read, when the data folder named by
///   `LODESTAR_DATA_DIR` or `worker --data` is the service's, when the
///   config file is inside it, or when the MSI's executable reads
///   `config.toml` there;
/// - after loading, when the config's data folder is the service's, before
///   any command opens the store.
///
/// A folder that isn't used isn't checked: `--config` with a `data_dir`
/// elsewhere works whatever state the service's folder is in. The CLI
/// never sets the folder's permissions. Commands that write there
/// (backup's `backups`, vacuum, reindex, rollback, rollup, import,
/// settings, companion) add files that inherit them.
pub(crate) fn load_config(
    command: &Command,
    config_path: Option<&Path>,
    install: &Install,
    env: impl Fn(&str) -> Option<String>,
    check: impl FnOnce(&Path) -> FolderCheck,
) -> Result<(Config, Option<PathBuf>), LoadError> {
    let data = match command {
        Command::Worker(a) => a.data.as_ref().and_then(|d| d.to_str()).map(str::to_owned),
        _ => None,
    };
    let env = |k: &str| match (k, &data) {
        ("LODESTAR_DATA_DIR", Some(d)) => Some(d.clone()),
        _ => env(k),
    };
    let mut guard = ServiceGuard::new(install, check);
    let msi = msi_data_dir(install)
        .filter(|_| env("LODESTAR_DATA_DIR").is_none())
        .filter(|_| {
            !matches!(
                command,
                Command::Worker(_) | Command::Serve { .. } | Command::Healthcheck
            )
        });
    guard.before(
        config_path,
        env("LODESTAR_DATA_DIR").as_deref(),
        msi.is_some() && config_path.is_none(),
    )?;
    let (config, file) = match msi {
        None => {
            let file = config_file(config_path, env);
            (Config::load(config_path, env)?, file)
        }
        Some(dir) => {
            let file =
                crate::windows::service::config_path(config_path.map(Path::to_path_buf), &dir, env);
            let mut config = match (&file, dir.to_str()) {
                // No config.toml there: loaded as if pointed at that
                // folder, so `data\config.toml` in the working folder is
                // never read, and not counted as set by the environment.
                (None, Some(d)) => {
                    let mut c = Config::load(None, |k| match k {
                        "LODESTAR_DATA_DIR" => Some(d.to_owned()),
                        _ => env(k),
                    })?;
                    c.layers.env.retain(|v| *v != "LODESTAR_DATA_DIR");
                    c
                }
                _ => Config::load(file.as_deref(), env)?,
            };
            crate::windows::service::default_data_dir_unless_set(&mut config, &dir);
            (config, file)
        }
    };
    guard.after(&config.data_dir)?;
    Ok((config, file))
}

/// Parses the process's arguments; on bad ones (or `--help`,
/// `--version`) clap prints and exits (status 2 for errors).
pub fn parse() -> Cli {
    Cli::parse()
}

/// Runs a maintenance command; `serve`, `healthcheck` and `lists` are the
/// binary's own (main.rs).
/// `dropped` is what the PUID/PGID drop did, logged once the config (and
/// so the log filter) is loaded.
pub fn run(command: Command, config_path: Option<&Path>, dropped: &privdrop::Report) -> ExitCode {
    // Reads no config: nothing from the data folder decides where it looks.
    if let Command::UpdateHelper(a) = command {
        return update_helper::run(&a, config_path);
    }
    let install = Install::here();
    let loaded = load_config(
        &command,
        config_path,
        &install,
        |k| std::env::var(k).ok(),
        check_this_service_dir,
    );
    let (config, file) = match loaded {
        Ok(c) => c,
        Err(e) => return report(&e),
    };
    crate::settings::log::init(&config.log);
    dropped.log(&config.data_dir);
    // Its own exit statuses: a bad setting or value is 2, as clap's are.
    if let Command::Settings(a) = command {
        return settings::run(&config, a);
    }
    let (name, outcome) = match command {
        Command::Backup(a) => (
            "backup",
            backup::run(&config, file.as_deref(), a.to.as_deref()),
        ),
        Command::Rollback(a) => (
            "rollback",
            rollback::run(
                &config,
                rollback::Options {
                    snapshot: a.snapshot.as_deref(),
                    yes: a.yes,
                    update: a.update,
                },
            ),
        ),
        Command::Vacuum => ("vacuum", vacuum::run(&config)),
        Command::Reindex => ("reindex", reindex::run(&config)),
        Command::Stats(a) => ("stats", stats::run(&config, a.json)),
        Command::Rollup(a) => ("rollup", rollup::run(&config, a.since)),
        Command::Import(a) => ("import", import::run(&config, a)),
        Command::ImportBitmagnet(a) => ("import-bitmagnet", import::legacy(&config, a)),
        Command::Worker(a) => ("worker", worker::run(&config, a)),
        Command::Companion(a) => ("companion", companion::run(&config, a)),
        Command::Serve { .. }
        | Command::Healthcheck
        | Command::Lists
        | Command::UpdateHelper(_) => {
            unreachable!("main.rs runs these")
        }
        Command::Settings(_) => {
            unreachable!("run above")
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e @ Failure::Refused(_)) => {
            eprintln!("lodestar {name}: {e}");
            ExitCode::from(3)
        }
        Err(e @ Failure::Failed(_)) => {
            eprintln!("lodestar {name}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The `config.toml` that [`Config::load`] reads: `--config`, else
/// `$LODESTAR_DATA_DIR/config.toml` or `data/config.toml` if present.
fn config_file(explicit: Option<&Path>, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    match explicit {
        Some(p) => Some(p.to_path_buf()),
        None => {
            let dir = env("LODESTAR_DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from);
            Some(dir.join("config.toml")).filter(|p| p.is_file())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// X-cli-win-data-dir: an install whose executable is the MSI's, in
    /// folders under `root` (no real Program Files or ProgramData).
    fn msi(root: &Path) -> Install {
        let pf = root.join("Program Files");
        Install::windows(
            Some(pf.join("Lodestar").join("lodestar.exe")),
            Some(pf),
            Some(root.join("ProgramData").to_string_lossy().into_owned()),
        )
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    /// A TOML string for a path, on any platform.
    fn toml_path(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    /// The MSI's executable finds the service's folder with nothing set,
    /// reads its config.toml, and lets that file, `--config`,
    /// `LODESTAR_DATA_DIR` and `worker --data` choose another, in that
    /// order of strength; any other executable keeps `data`.
    #[test]
    fn the_msi_executable_finds_the_services_data_folder() {
        let root = tempfile::tempdir().unwrap();
        let install = msi(root.path());
        let service_dir = root.path().join("ProgramData").join("Lodestar");
        assert_eq!(msi_data_dir(&install), Some(service_dir.clone()));
        let none = env_of(&[]);

        // Nothing set and no config.toml there: the service's folder.
        let (c, file) = load_config(&Command::Vacuum, None, &install, &none, private).unwrap();
        assert_eq!((c.data_dir.clone(), file), (service_dir.clone(), None));
        assert!(!c.layers.in_env("LODESTAR_DATA_DIR"));

        // Its config.toml is read; a data_dir there wins.
        std::fs::create_dir_all(&service_dir).unwrap();
        let moved = root.path().join("D").join("Lodestar");
        let config_toml = service_dir.join("config.toml");
        std::fs::write(
            &config_toml,
            format!("data_dir = \"{}\"\n", toml_path(&moved)),
        )
        .unwrap();
        let (c, file) = load_config(&Command::Vacuum, None, &install, &none, private).unwrap();
        assert_eq!((c.data_dir, file), (moved, Some(config_toml.clone())));

        // `--config` without a data_dir: the service's folder still.
        let other = root.path().join("other.toml");
        std::fs::write(&other, "").unwrap();
        let (c, file) =
            load_config(&Command::Vacuum, Some(&other), &install, &none, private).unwrap();
        assert_eq!((c.data_dir, file), (service_dir.clone(), Some(other)));

        // LODESTAR_DATA_DIR wins, and the service's config.toml isn't read.
        let chosen = root.path().join("chosen");
        let env = env_of(&[("LODESTAR_DATA_DIR", chosen.to_str().unwrap())]);
        let (c, file) = load_config(&Command::Vacuum, None, &install, &env, unasked).unwrap();
        assert_eq!((c.data_dir, file), (chosen, None));

        // `worker --data` wins too, and `worker` never defaults to the
        // service's folder: without `--data` it keeps `data`.
        let mine = root.path().join("worker");
        let worker = |data: Option<PathBuf>| {
            Command::Worker(worker::WorkerArgs {
                join: None,
                token_file: None,
                data,
                rotate_key: false,
                join_only: false,
            })
        };
        let (c, _) =
            load_config(&worker(Some(mine.clone())), None, &install, &none, unasked).unwrap();
        assert_eq!(c.data_dir, mine);
        let (c, _) = load_config(&worker(None), None, &install, &none, unasked).unwrap();
        assert_eq!(c.data_dir, PathBuf::from("data"));
    }

    /// A check that finds the service's folder private.
    fn private(_: &Path) -> FolderCheck {
        Ok(())
    }

    /// A check that must not be asked: the service's folder isn't used.
    fn unasked(dir: &Path) -> FolderCheck {
        panic!("the service's folder {} was checked", dir.display())
    }

    /// S8-07: the service's folder is used only once it's found private,
    /// as the service judges it; otherwise nothing in it is read.
    #[test]
    fn the_services_folder_is_used_only_when_private() {
        use crate::windows::acl::{Inspection, Item, Ops};
        use std::cell::RefCell;

        /// The service's SID and its folder as the installer makes it.
        const OWN: &str = "S-1-5-80-1-2-3-4-5";
        const PRIVATE: &str =
            "O:BAG:SYD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;S-1-5-80-1-2-3-4-5)";
        /// Made by a standard user: theirs, and open to every user.
        const PLANTED: &str = "O:S-1-5-21-9-9-9-1001G:S-1-5-21-9-9-9-513D:AI(A;OICIID;FA;;;SY)\
                               (A;OICIID;FA;;;BA)(A;OICIID;0x1200a9;;;BU)";

        struct Fake {
            seen: Option<Inspection>,
            calls: RefCell<Vec<&'static str>>,
        }
        impl Ops for Fake {
            fn inspect(&self, _: &Path) -> std::io::Result<Option<Inspection>> {
                self.calls.borrow_mut().push("inspect");
                Ok(self.seen.clone())
            }
            fn create(&self, _: &Path) -> std::io::Result<()> {
                self.calls.borrow_mut().push("create");
                Ok(())
            }
        }
        let fake = |sddl: Option<&str>, entries: Vec<Item>| Fake {
            seen: sddl.map(|s| Inspection {
                folder: Item {
                    name: "Lodestar".into(),
                    link: false,
                    sddl: s.into(),
                },
                entries,
                links: Vec::new(),
            }),
            calls: RefCell::new(Vec::new()),
        };

        let root = tempfile::tempdir().unwrap();
        let install = msi(root.path());
        let service_dir = root.path().join("ProgramData").join("Lodestar");
        // A broken config.toml: a refusal below (rather than a config
        // error) proves the check ran before anything in the folder was
        // read.
        std::fs::create_dir_all(&service_dir).unwrap();
        std::fs::write(service_dir.join("config.toml"), "not toml [").unwrap();
        let load = |ops: &Fake| {
            load_config(&Command::Vacuum, None, &install, env_of(&[]), |d| {
                check_service_dir(d, Some(OWN), ops)
            })
        };

        // Not private: refused in the service's words, nothing created.
        let planted = fake(Some(PLANTED), Vec::new());
        match load(&planted) {
            Err(LoadError::Folder(Failure::Refused(why))) => {
                assert!(why.contains("isn't private"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(*planted.calls.borrow(), ["inspect"]);

        // A private folder with a file another user put in it: refused.
        let foreign = Item {
            name: "config.toml".into(),
            link: false,
            sddl: "O:S-1-5-21-9-9-9-1001D:AI(A;ID;FA;;;SY)".into(),
        };
        let planted_file = fake(Some(PRIVATE), vec![foreign]);
        assert!(matches!(
            load(&planted_file),
            Err(LoadError::Folder(Failure::Refused(_)))
        ));

        // Missing: no store there, and nothing is created.
        let missing = fake(None, Vec::new());
        match check_service_dir(&service_dir, Some(OWN), &missing) {
            Err(Failure::Failed(why)) => {
                assert!(why.starts_with("no store at"), "{why}");
                assert!(why.contains(&service_dir.display().to_string()), "{why}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(*missing.calls.borrow(), ["inspect"]);

        // Private: its config.toml is read (and here it's broken).
        let ok = fake(Some(PRIVATE), Vec::new());
        assert!(matches!(load(&ok), Err(LoadError::Config(_))));
        std::fs::write(service_dir.join("config.toml"), "").unwrap();
        let (c, _) = load(&ok).unwrap();
        assert_eq!(c.data_dir, service_dir);
        assert!(!ok.calls.borrow().contains(&"create"));
    }

    /// Any other executable is left as before: the portable zip's (beside
    /// its own folder), one elsewhere in Program Files, and every one on
    /// a platform with no MSI.
    #[test]
    fn other_executables_keep_data_in_the_working_folder() {
        let root = tempfile::tempdir().unwrap();
        let mut portable = msi(root.path());
        portable.exe = Some(root.path().join("Lodestar portable").join("lodestar.exe"));
        let mut elsewhere = msi(root.path());
        elsewhere.exe = Some(
            root.path()
                .join("Program Files")
                .join("Other")
                .join("lodestar.exe"),
        );
        let mut no_program_files = msi(root.path());
        no_program_files.program_files = None;
        for install in [portable, elsewhere, no_program_files, Install::default()] {
            assert_eq!(msi_data_dir(&install), None, "{install:?}");
            let (c, _) =
                load_config(&Command::Vacuum, None, &install, env_of(&[]), unasked).unwrap();
            assert_eq!(c.data_dir, PathBuf::from("data"), "{install:?}");
        }
        // This test binary is never the MSI's, on any platform.
        assert_eq!(msi_data_dir(&Install::here()), None);
    }

    /// A check that refuses, as for a folder that isn't private.
    fn refuse(_: &Path) -> FolderCheck {
        Err(Failure::Refused("isn't private".into()))
    }

    fn refused(r: Result<(Config, Option<PathBuf>), LoadError>) -> bool {
        matches!(r, Err(LoadError::Folder(Failure::Refused(_))))
    }

    /// S8-07: the service's folder is checked whenever it's used, from any
    /// executable and however it's named; one that isn't used isn't.
    #[test]
    fn the_services_folder_is_checked_however_its_named() {
        let root = tempfile::tempdir().unwrap();
        let install = msi(root.path());
        let mut portable = msi(root.path());
        portable.exe = Some(root.path().join("Lodestar portable").join("lodestar.exe"));
        let service_dir = root.path().join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&service_dir).unwrap();
        let svc = service_dir.to_str().unwrap();
        let none = env_of(&[]);

        // (a) LODESTAR_DATA_DIR naming it, from the MSI's or any executable.
        let named = env_of(&[("LODESTAR_DATA_DIR", svc)]);
        for i in [&install, &portable] {
            assert!(refused(load_config(
                &Command::Vacuum,
                None,
                i,
                &named,
                refuse
            )));
            let (c, _) = load_config(&Command::Vacuum, None, i, &named, private).unwrap();
            assert_eq!(c.data_dir, service_dir);
        }

        // (b) `worker --data` naming it.
        let worker = Command::Worker(worker::WorkerArgs {
            join: None,
            token_file: None,
            data: Some(service_dir.clone()),
            rotate_key: false,
            join_only: false,
        });
        assert!(refused(load_config(&worker, None, &install, &none, refuse)));

        // (c) `--config` from elsewhere whose data_dir is the service's,
        // from any executable: checked before the store is opened.
        let to_service = root.path().join("to-service.toml");
        std::fs::write(
            &to_service,
            format!("data_dir = \"{}\"\n", toml_path(&service_dir)),
        )
        .unwrap();
        for i in [&install, &portable] {
            assert!(refused(load_config(
                &Command::Vacuum,
                Some(&to_service),
                i,
                &none,
                refuse
            )));
        }

        // A config file inside it, even naming another data folder: checked
        // before it's read.
        let elsewhere = root.path().join("D").join("Lodestar");
        let inside = service_dir.join("other.toml");
        std::fs::write(
            &inside,
            format!("data_dir = \"{}\"\n", toml_path(&elsewhere)),
        )
        .unwrap();
        assert!(refused(load_config(
            &Command::Vacuum,
            Some(&inside),
            &portable,
            &none,
            refuse
        )));

        // I1: `--config` naming another data folder, from the MSI's
        // executable, works whatever state the service's folder is in.
        let away = root.path().join("away.toml");
        std::fs::write(&away, format!("data_dir = \"{}\"\n", toml_path(&elsewhere))).unwrap();
        let (c, file) =
            load_config(&Command::Vacuum, Some(&away), &install, &none, unasked).unwrap();
        assert_eq!((c.data_dir, file), (elsewhere.clone(), Some(away)));
        // So does LODESTAR_DATA_DIR naming another folder.
        let other = env_of(&[("LODESTAR_DATA_DIR", elsewhere.to_str().unwrap())]);
        let (c, _) = load_config(&Command::Vacuum, None, &install, &other, unasked).unwrap();
        assert_eq!(c.data_dir, elsewhere);
    }

    /// S8-07: with `%ProgramData%` unset, the service's folder is the
    /// service's own fallback, and it's checked: never skipped.
    #[test]
    fn an_unset_program_data_still_checks_the_services_folder() {
        let root = tempfile::tempdir().unwrap();
        let pf = root.path().join("Program Files");
        let install = Install::windows(
            Some(pf.join("Lodestar").join("lodestar.exe")),
            Some(pf),
            None,
        );
        assert_eq!(
            install.service_dir,
            Some(crate::windows::service::default_data_dir(None))
        );
        assert_eq!(msi_data_dir(&install), install.service_dir);
        let called = std::cell::Cell::new(false);
        let r = load_config(&Command::Vacuum, None, &install, env_of(&[]), |d| {
            called.set(true);
            refuse(d)
        });
        assert!(called.get() && refused(r));
    }

    /// S8-07: hand-run `serve` and `healthcheck` use the service's folder
    /// only once it's found private, however it's named; they never
    /// default to it, and another folder isn't checked.
    #[test]
    fn hand_run_serve_checks_the_services_folder() {
        let root = tempfile::tempdir().unwrap();
        let install = msi(root.path());
        let service_dir = root.path().join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&service_dir).unwrap();
        // Never read unless the folder is found private.
        std::fs::write(service_dir.join("config.toml"), "not toml [").unwrap();
        let none = env_of(&[]);
        let refused =
            |r: Result<Config, LoadError>| matches!(r, Err(LoadError::Folder(Failure::Refused(_))));

        // Named by LODESTAR_DATA_DIR.
        let named = env_of(&[("LODESTAR_DATA_DIR", service_dir.to_str().unwrap())]);
        assert!(refused(load_serve_config(None, &install, &named, refuse)));
        // A config file inside it.
        let inside = service_dir.join("config.toml");
        assert!(refused(load_serve_config(
            Some(&inside),
            &install,
            &none,
            refuse
        )));
        // A config file elsewhere whose data_dir names it.
        let to_service = root.path().join("to-service.toml");
        std::fs::write(
            &to_service,
            format!("data_dir = \"{}\"\n", toml_path(&service_dir)),
        )
        .unwrap();
        assert!(refused(load_serve_config(
            Some(&to_service),
            &install,
            &none,
            refuse
        )));

        // Another folder, or nothing set (even from the MSI's executable):
        // not checked, and `serve` keeps `data` as before.
        let other = root.path().join("D").join("Lodestar");
        let elsewhere = env_of(&[("LODESTAR_DATA_DIR", other.to_str().unwrap())]);
        let c = load_serve_config(None, &install, &elsewhere, unasked).unwrap();
        assert_eq!(c.data_dir, other);
        let c = load_serve_config(None, &install, &none, unasked).unwrap();
        assert_eq!(c.data_dir, PathBuf::from("data"));
    }

    /// An empty data folder is the working folder, so it's the service's
    /// when the working folder is.
    #[test]
    fn an_empty_data_folder_is_the_working_folder() {
        let here = std::env::current_dir().unwrap();
        let install = Install {
            service_dir: Some(here),
            ..Install::default()
        };
        let mut guard = ServiceGuard::new(&install, refuse);
        assert!(matches!(
            guard.after(Path::new("")),
            Err(LoadError::Folder(Failure::Refused(_)))
        ));
        let mut guard = ServiceGuard::new(&install, refuse);
        assert!(guard.before(None, Some(""), false).is_err());
        let mut guard = ServiceGuard::new(&install, unasked);
        assert!(guard.after(Path::new("elsewhere")).is_ok());
    }

    /// The MSI's executable is matched as Windows compares paths: by case.
    #[test]
    fn the_msi_executable_matches_whatever_its_case() {
        let root = tempfile::tempdir().unwrap();
        let mut install = msi(root.path());
        let pf = install.program_files.clone().unwrap();
        install.exe = Some(pf.join("LODESTAR").join("Lodestar.EXE"));
        // The same on every platform (`same_dir` compares as Windows does);
        // off Windows `Install::here` names no executable, so it never applies.
        assert!(msi_data_dir(&install).is_some());
    }

    /// No store: the message names the folder looked in and how to point
    /// at another.
    #[test]
    fn no_store_says_where_it_looked() {
        let db = Path::new("data").join("lodestar.db");
        let words = util::no_store(&db);
        assert!(words.starts_with("no store at"), "{words}");
        assert!(
            words.contains(&format!("data folder {}", Path::new("data").display())),
            "{words}"
        );
        assert!(
            words.contains("LODESTAR_DATA_DIR") && words.contains("--config"),
            "{words}"
        );
    }

    fn args(s: &str) -> Result<Cli, clap::error::ErrorKind> {
        Cli::try_parse_from(std::iter::once("lodestar").chain(s.split_whitespace()))
            .map_err(|e| e.kind())
    }

    const SERVE: Command = Command::Serve {
        fixture: None,
        child: false,
    };

    fn cmd(s: &str) -> (Option<PathBuf>, Option<Command>) {
        let c = args(s).unwrap();
        (c.config, c.command)
    }

    /// #619's review: an install needs the shown SHA-256, never a bare
    /// yes; a removal without `--yes` only shows what it would delete.
    #[test]
    fn companion_confirmation_is_explicit() {
        use companion::{CompanionArgs, CompanionCommand};
        assert_eq!(
            args("companion install qbittorrent --yes").unwrap_err(),
            clap::error::ErrorKind::UnknownArgument
        );
        let hex = "ab".repeat(32);
        assert_eq!(
            cmd(&format!("companion install qbittorrent --sha256 {hex}")).1,
            Some(Command::Companion(CompanionArgs {
                command: CompanionCommand::Install {
                    id: "qbittorrent".into(),
                    sha256: Some(hex),
                }
            }))
        );
        assert_eq!(
            cmd("companion remove x").1,
            Some(Command::Companion(CompanionArgs {
                command: CompanionCommand::Remove {
                    id: "x".into(),
                    yes: false
                }
            }))
        );
    }

    #[test]
    fn serve_stays_the_default() {
        assert_eq!(cmd(""), (None, None));
        assert_eq!(cmd("serve"), (None, Some(SERVE)));
        assert_eq!(
            cmd("serve --fixture").1,
            Some(Command::Serve {
                fixture: Some(None),
                child: false
            })
        );
        assert_eq!(
            cmd("serve --fixture /tmp/fx --config c.toml").1,
            Some(Command::Serve {
                fixture: Some(Some("/tmp/fx".into())),
                child: false
            })
        );
        assert_eq!(cmd("--config /x.toml"), (Some("/x.toml".into()), None));
        assert_eq!(
            cmd("serve --config /x.toml"),
            (Some("/x.toml".into()), Some(SERVE))
        );
        assert_eq!(
            cmd("--config c.toml healthcheck"),
            (Some("c.toml".into()), Some(Command::Healthcheck))
        );
    }

    #[test]
    fn parses_the_existing_commands() {
        assert_eq!(cmd("lists").1, Some(Command::Lists));
        assert_eq!(
            cmd("import-bitmagnet --host db.lan --password-file pw --limit 10").1,
            Some(Command::ImportBitmagnet(ImportBitmagnetArgs {
                host: "db.lan".into(),
                port: 5432,
                dbname: "bitmagnet".into(),
                user: "bitmagnet_reader".into(),
                password_file: Some("pw".into()),
                ca_file: None,
                allow_plaintext_loopback: false,
                limit: Some(10)
            }))
        );
        use clap::error::ErrorKind;
        // No URL and no --password: argv is visible to other processes.
        assert_eq!(
            args("import-bitmagnet postgres://u:p@x/bm").err(),
            Some(ErrorKind::UnknownArgument)
        );
        assert_eq!(
            args("import-bitmagnet --password p").err(),
            Some(ErrorKind::UnknownArgument)
        );
        assert_eq!(
            args("import-bitmagnet --ca-file a --allow-plaintext-loopback").err(),
            Some(ErrorKind::ArgumentConflict)
        );
        assert_eq!(
            args("import-bitmagnet --limit ten").err(),
            Some(ErrorKind::ValueValidation)
        );
        assert_eq!(args("--version").err(), Some(ErrorKind::DisplayVersion));
        assert_eq!(args("-V").err(), Some(ErrorKind::DisplayVersion));
        assert_eq!(args("serve -h").err(), Some(ErrorKind::DisplayHelp));
        assert_eq!(args("--help").err(), Some(ErrorKind::DisplayHelp));
        assert!(args("serve serve").is_err());
        assert!(args("serve extra").is_err());
        assert!(args("--config").is_err());
        assert!(args("frobnicate").is_err());
    }

    #[test]
    fn parses_the_maintenance_commands() {
        assert_eq!(
            cmd("backup").1,
            Some(Command::Backup(BackupArgs { to: None }))
        );
        assert_eq!(
            cmd("backup --to /mnt/user/backups").1,
            Some(Command::Backup(BackupArgs {
                to: Some("/mnt/user/backups".into())
            }))
        );
        assert_eq!(
            cmd("rollback").1,
            Some(Command::Rollback(RollbackArgs {
                snapshot: None,
                yes: false,
                update: false
            }))
        );
        assert_eq!(
            cmd("rollback lodestar.pre-v4.db --yes --config c.toml"),
            (
                Some("c.toml".into()),
                Some(Command::Rollback(RollbackArgs {
                    snapshot: Some("lodestar.pre-v4.db".into()),
                    yes: true,
                    update: false
                }))
            )
        );
        assert_eq!(cmd("vacuum").1, Some(Command::Vacuum));
        assert_eq!(cmd("reindex").1, Some(Command::Reindex));
        assert_eq!(
            cmd("stats --json").1,
            Some(Command::Stats(StatsArgs { json: true }))
        );
        assert_eq!(
            cmd("rollup --repair").1,
            Some(Command::Rollup(RollupArgs {
                repair: true,
                since: None
            }))
        );
        assert_eq!(
            cmd("rollup --repair --since 90d").1,
            Some(Command::Rollup(RollupArgs {
                repair: true,
                since: Some(90)
            }))
        );
        use clap::error::ErrorKind;
        assert_eq!(
            args("rollup").err(),
            Some(ErrorKind::MissingRequiredArgument)
        );
        assert_eq!(
            args("rollup --repair --since 90").err(),
            Some(ErrorKind::ValueValidation)
        );
        assert_eq!(
            cmd("rollback --update").1,
            Some(Command::Rollback(RollbackArgs {
                snapshot: None,
                yes: false,
                update: true
            }))
        );
        assert!(args("rollback v4 --update").is_err());
        assert_eq!(
            cmd("serve --child").1,
            Some(Command::Serve {
                fixture: None,
                child: true
            })
        );
        assert!(args("serve --fixture --child").is_err());
        assert!(args("vacuum --yes").is_err());
        assert!(args("rollback a b").is_err());
    }

    #[test]
    fn the_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
