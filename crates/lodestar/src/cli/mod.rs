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
//!   `import-bitmagnet` kept as its old name.
//!
//! Each refuses while B-update's rollback journal exists (§12.2), and the
//! offline ones refuse while anything has the store open ([`lock`]).

mod backup;
mod import;
mod lock;
mod reindex;
mod rollback;
mod rollup;
mod stats;
pub(crate) mod util;
mod vacuum;

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
    let config = match Config::load(config_path, |k| std::env::var(k).ok()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config: {e}");
            return ExitCode::FAILURE;
        }
    };
    crate::settings::log::init(&config.log);
    dropped.log(&config.data_dir);
    let (name, outcome) = match command {
        Command::Backup(a) => (
            "backup",
            backup::run(
                &config,
                config_file(config_path).as_deref(),
                a.to.as_deref(),
            ),
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
        Command::Serve { .. } | Command::Healthcheck | Command::Lists => {
            unreachable!("main.rs runs these")
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
fn config_file(explicit: Option<&Path>) -> Option<PathBuf> {
    match explicit {
        Some(p) => Some(p.to_path_buf()),
        None => {
            let dir = std::env::var_os("LODESTAR_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("data"));
            Some(dir.join("config.toml")).filter(|p| p.is_file())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
