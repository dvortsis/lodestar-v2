//! `lodestar worker` (architecture §19.6, workers plan WK-2).
//!
//! - `lodestar worker --join <url> --token-file <path|->` joins with a
//!   one-time token, saves the credentials and runs.
//! - `lodestar worker [--data <dir>]` runs from the saved credentials.
//! - `lodestar worker --rotate-key` (worker stopped) seals the credentials
//!   under a new secrets key.
//!
//! The token comes from a file or stdin (`-`), never the command line,
//! where it would show in process listings and shell history (§13,
//! V6-cluster2). For the container templates (plan Q2, owner §2.47) a
//! first join may also take it from `LODESTAR_TOKEN`, with the address in
//! `--join` or `LODESTAR_JOIN`: only while no credentials exist. Once the
//! worker has joined the variable is ignored, with a warning to remove
//! it. Its value is never printed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Args;
use lodestar_cluster::{JoinToken, SystemClock, read_join_token, read_join_token_file};

use super::util::{Failure, Outcome, failed};
use crate::config::Config;
use crate::worker::session::{Exit, Report, Settings, Tcp, Worker, random_jitter, sleep};
use crate::worker::{self, JoinFailure, Layout};

#[derive(Debug, Args, PartialEq, Eq)]
pub struct WorkerArgs {
    /// Join the coordinator at this address (https://host:port; the port
    /// defaults to 8091), then run
    #[arg(long, value_name = "URL")]
    pub join: Option<String>,
    /// Read the one-time join token from this file, or from stdin with
    /// `-`. It's never taken from the command line (a first join may take
    /// it from LODESTAR_TOKEN instead)
    #[arg(long, value_name = "PATH", requires = "join")]
    pub token_file: Option<PathBuf>,
    /// The worker's data folder (default: $LODESTAR_DATA_DIR or data). Use
    /// a folder of its own, not a coordinator's
    #[arg(long, value_name = "DIR")]
    pub data: Option<PathBuf>,
    /// Seal the saved credentials under a new secrets key, then retire the
    /// old key (worker stopped)
    #[arg(long, conflicts_with_all = ["join", "token_file"])]
    pub rotate_key: bool,
}

/// What to tell the user to run to join again.
const JOIN_AGAIN: &str = "make a new join token in Settings → Workers → Add worker, then run \
     `lodestar worker --join https://<coordinator>:8091 --token-file <file>`";

/// Where a join's token comes from. Its `Debug` never shows the token.
#[derive(PartialEq, Eq)]
enum TokenSource {
    File(PathBuf),
    /// `LODESTAR_TOKEN`, first join only.
    Env(String),
}

impl std::fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenSource::File(p) => f.debug_tuple("File").field(p).finish(),
            TokenSource::Env(_) => f.write_str("Env(***)"),
        }
    }
}

/// Whether to join first, and with what.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    /// The `--join` text and the token's source.
    join: Option<(String, TokenSource)>,
    warnings: Vec<&'static str>,
}

const ENV_TOKEN_STALE: &str = "LODESTAR_TOKEN is still set, but this worker has already joined, \
     so it's ignored. Remove it from the container's settings: a token works only once.";
const JOIN_IGNORED: &str = "--join is ignored: this folder has already joined a coordinator.";
const ENV_TOKEN_UNUSED: &str = "LODESTAR_TOKEN is ignored: --token-file was given.";
/// The same two, said by an image `prepare` started again without the
/// token: only `LODESTAR_TOKEN_IGNORED` says it was set, and someone may
/// have set that by hand (WK-2's review), so the line says what to remove
/// in either case.
const ENV_TOKEN_STALE_RESTARTED: &str = "LODESTAR_TOKEN is still set, but this worker has \
     already joined, so it's ignored. Remove it from the container's settings: a token works \
     only once. If LODESTAR_TOKEN isn't set, remove LODESTAR_TOKEN_IGNORED instead: Lodestar \
     sets that one itself.";
const ENV_TOKEN_UNUSED_RESTARTED: &str = "LODESTAR_TOKEN is ignored: --token-file was given. \
     If LODESTAR_TOKEN isn't set, remove LODESTAR_TOKEN_IGNORED instead: Lodestar sets that \
     one itself.";

/// Decides whether to join: `--token-file` always does (with `--join`);
/// otherwise `LODESTAR_TOKEN` does only while the folder hasn't joined.
fn plan(args: &WorkerArgs, env: &dyn Fn(&str) -> Option<String>, joined: bool) -> Outcome<Plan> {
    let set = |k: &str| env(k).filter(|v| !v.trim().is_empty());
    let env_token = set("LODESTAR_TOKEN");
    let mut warnings = Vec::new();
    if let Some(path) = &args.token_file {
        if env_token.is_some() {
            warnings.push(ENV_TOKEN_UNUSED);
        } else if set(TOKEN_IGNORED).is_some() {
            warnings.push(ENV_TOKEN_UNUSED_RESTARTED);
        }
        let url = args.join.clone().ok_or_else(|| {
            Failure::Failed("--token-file needs --join <coordinator address>".to_owned())
        })?;
        return Ok(Plan {
            join: Some((url, TokenSource::File(path.clone()))),
            warnings,
        });
    }
    // A process image started again without the token (`prepare`) still
    // says it was ignored.
    let ignored = env_token.is_none() && set(TOKEN_IGNORED).is_some();
    match env_token {
        _ if ignored && joined => {
            warnings.push(ENV_TOKEN_STALE_RESTARTED);
            if args.join.is_some() {
                warnings.push(JOIN_IGNORED);
            }
            Ok(Plan {
                join: None,
                warnings,
            })
        }
        Some(_) if joined => {
            warnings.push(ENV_TOKEN_STALE);
            if args.join.is_some() {
                warnings.push(JOIN_IGNORED);
            }
            Ok(Plan {
                join: None,
                warnings,
            })
        }
        Some(token) => {
            let url = args
                .join
                .clone()
                .or_else(|| set("LODESTAR_JOIN"))
                .ok_or_else(|| {
                    Failure::Failed(
                        "LODESTAR_TOKEN is set, but not the coordinator's address: set \
                     LODESTAR_JOIN (or --join), like https://192.168.1.10:8091"
                            .to_owned(),
                    )
                })?;
            Ok(Plan {
                join: Some((url, TokenSource::Env(token))),
                warnings,
            })
        }
        None if args.join.is_some() => Err(Failure::Failed(
            "--join needs the token: --token-file <file>, or --token-file - to read it \
             from stdin"
                .to_owned(),
        )),
        None => Ok(Plan {
            join: None,
            warnings,
        }),
    }
}

/// Set (to `1`) on the image `prepare` starts again without
/// `LODESTAR_TOKEN`, so it still prints the warning to remove it.
const TOKEN_IGNORED: &str = "LODESTAR_TOKEN_IGNORED";

/// What `main` does for `lodestar worker` before the privilege drop, while
/// the process is still single-threaded (the folder is `--data`, else
/// `LODESTAR_DATA_DIR`, else `data`, as for the drop; a `data_dir` in
/// config.toml isn't read here):
/// - refuses a `--data` that isn't UTF-8, or that names another folder than
///   `LODESTAR_DATA_DIR`, so the folder the drop owns is the one written;
/// - when `LODESTAR_TOKEN` would only be ignored (a `--token-file` wins, or
///   the folder has already joined), starts the same command again without it (Unix `exec`, same
///   process). The ignored token was never redeemed, so it stays valid for
///   its 15 minutes; this keeps it out of `/proc/<pid>/environ`. (Clearing
///   the variable in place would need `unsafe`, and the kernel's copy of the
///   start-up environment would keep it anyway.)
///
/// Returns the `--data` folder as text, for the drop and the config.
pub fn prepare(args: &WorkerArgs) -> Result<Option<String>, std::process::ExitCode> {
    let refuse = |m: String| {
        eprintln!("lodestar worker: {m}");
        Err(std::process::ExitCode::from(2))
    };
    let data = match &args.data {
        None => None,
        Some(d) => match d.to_str() {
            Some(t) => Some(t.to_owned()),
            None => return refuse("--data must be a path in UTF-8.".to_owned()),
        },
    };
    let var = std::env::var_os("LODESTAR_DATA_DIR");
    if let (Some(flag), Some(var)) = (&data, &var)
        && Path::new(flag) != Path::new(var)
    {
        return refuse(format!(
            "--data {flag} and LODESTAR_DATA_DIR={} name different folders. Set only one, \
             or the same folder in both.",
            Path::new(var).display()
        ));
    }
    #[cfg(unix)]
    {
        let folder = data
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| var.map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("data"));
        // Blank counts as unset, as in `plan`.
        let token = std::env::var("LODESTAR_TOKEN")
            .ok()
            .filter(|v| !v.trim().is_empty());
        // Ignored when a token file wins, or once the folder has joined.
        if token.is_some() && (args.token_file.is_some() || Layout::new(&folder).is_joined()) {
            use std::os::unix::process::CommandExt;
            // On Linux the running image itself: the same file even if the
            // binary was replaced or removed meanwhile; argv[0] is kept.
            #[cfg(target_os = "linux")]
            let exe = Ok::<_, std::io::Error>(PathBuf::from("/proc/self/exe"));
            #[cfg(not(target_os = "linux"))]
            let exe = std::env::current_exe();
            let mut args_os = std::env::args_os();
            let argv0 = args_os.next();
            let e = exe.map(|exe| {
                let mut cmd = std::process::Command::new(exe);
                if let Some(argv0) = argv0 {
                    cmd.arg0(argv0);
                }
                cmd.args(args_os)
                    .env_remove("LODESTAR_TOKEN")
                    .env(TOKEN_IGNORED, "1")
                    .exec()
            });
            let kind = match e {
                Ok(e) | Err(e) => e.kind(),
            };
            return refuse(format!(
                "couldn't start again without LODESTAR_TOKEN ({kind}); remove it from the \
                 container's settings"
            ));
        }
    }
    Ok(data)
}

pub fn run(config: &Config, args: WorkerArgs) -> Outcome {
    run_with(config, args, &|k| std::env::var(k).ok())
}

fn run_with(config: &Config, args: WorkerArgs, env: &dyn Fn(&str) -> Option<String>) -> Outcome {
    let layout = Layout::new(args.data.clone().unwrap_or_else(|| config.data_dir.clone()));
    if layout.is_coordinators() {
        return Err(Failure::Refused(format!(
            "{} is a coordinator's data folder. Give the worker a folder of its own with --data.",
            layout.data.display()
        )));
    }
    let Some(_lock) = layout
        .lock()
        .map_err(failed("locking the worker's folder"))?
    else {
        return Err(Failure::Refused(format!(
            "another `lodestar worker` is using {}. Stop it first.",
            layout.data.display()
        )));
    };
    if args.rotate_key {
        return match worker::rotate_key(&layout).map_err(failed("rotating the secrets key"))? {
            true => {
                eprintln!("The secrets key was rotated and the worker's credentials re-sealed.");
                Ok(())
            }
            false => {
                eprintln!("The secrets key was rotated. This worker hasn't joined yet.");
                Ok(())
            }
        };
    }
    let report: Report = Arc::new(|line: &str| eprintln!("lodestar worker: {line}"));
    let joined = layout
        .load()
        .map_err(failed("reading the credentials"))?
        .map(|c| c.name().to_owned());
    let plan = plan(&args, env, joined.is_some())?;
    for w in &plan.warnings {
        report(w);
    }
    block_on(async move {
        let clock: Arc<dyn lodestar_cluster::Clock> = Arc::new(SystemClock);
        if let Some((url, source)) = plan.join {
            join(&layout, &url, source, joined, clock.clone(), &report).await?;
        }
        let creds = match layout.load().map_err(failed("reading the credentials"))? {
            Some(c) => c,
            None => {
                return Err(Failure::Failed(format!(
                    "this worker hasn't joined a coordinator yet: {JOIN_AGAIN}"
                )));
            }
        };
        let addr = layout
            .load_address()
            .map_err(failed("reading the coordinator's address"))?
            .ok_or_else(|| {
                Failure::Failed(format!(
                    "the coordinator's address is missing: {JOIN_AGAIN}"
                ))
            })?;
        report(&format!("connecting to {addr}"));
        let settings = Settings::default();
        // The worker's fetch budget (WK-10b): dicts up to `MAX_METADATA`,
        // reserved against the pool's 4 MiB before they're requested.
        let fetcher = Arc::new(lodestar_sources::pool::PeerWireFetcher(
            lodestar_peer_wire::FetchOptions {
                max_metadata: crate::worker::runtime::MAX_METADATA,
                ..lodestar_peer_wire::FetchOptions::default()
            },
        ));
        let runtime = crate::worker::runtime::Runtime::start(crate::worker::runtime::Config::new(
            creds.worker(),
            fetcher,
        ));
        let worker = Worker {
            layout,
            creds,
            connect: Tcp {
                addr,
                clock: clock.clone(),
                within: settings.connect_timeout,
            },
            clock,
            settings,
            report: report.clone(),
            jitter: random_jitter,
            wait: sleep(),
        };
        let exit = worker.run(runtime, crate::shutdown_signal()).await;
        finish(exit, &report)
    })
}

async fn join(
    layout: &Layout,
    url: &str,
    source: TokenSource,
    already: Option<String>,
    clock: Arc<dyn lodestar_cluster::Clock>,
    report: &Report,
) -> Outcome {
    let addr =
        worker::parse_coordinator(url).map_err(|e| Failure::Failed(format!("--join: {e}")))?;
    let token = match source {
        TokenSource::File(path) if path == Path::new("-") => {
            use std::io::IsTerminal;
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                report("paste the join token, press Enter, then Ctrl-D");
            }
            read_join_token(stdin.lock()).map_err(|e| Failure::Failed(e.to_string()))
        }
        TokenSource::File(path) => {
            read_join_token_file(&path).map_err(|e| Failure::Failed(e.to_string()))
        }
        TokenSource::Env(text) => {
            parse_and_wipe(text).map_err(|e| Failure::Failed(format!("LODESTAR_TOKEN: {e}")))
        }
    }?;
    match worker::join(layout, &addr, token, clock).await {
        Ok(creds) => {
            report(&format!("joined {addr} as worker {}", creds.name()));
            Ok(())
        }
        Err(e @ JoinFailure::Refused) => Err(Failure::Failed(match already {
            Some(name) => format!(
                "{e} This folder is already joined as worker {name}: to keep using that, \
                 start it without --join and --token-file."
            ),
            None => e.to_string(),
        })),
        Err(e) => Err(Failure::Failed(e.to_string())),
    }
}

/// Parses the variable's copy of the token, then zeroes that copy. (The
/// process's own environment still holds it; plan Q2 accepted that for a
/// first join.)
fn parse_and_wipe(text: String) -> Result<JoinToken, lodestar_cluster::TokenFormatError> {
    let mut bytes = text.into_bytes();
    let parsed = std::str::from_utf8(&bytes)
        .map_err(|_| lodestar_cluster::TokenFormatError::Encoding)
        .and_then(JoinToken::parse);
    bytes.fill(0);
    std::hint::black_box(&bytes);
    parsed
}

/// One plain line for how the worker stopped; an error for the ones the
/// user has to act on.
fn finish(exit: Exit, report: &Report) -> Outcome {
    match exit {
        Exit::Shutdown => {
            report("stopped");
            Ok(())
        }
        Exit::Drained => {
            report("drained: the coordinator has the work back; stopped");
            Ok(())
        }
        Exit::Revoked => Err(Failure::Failed(format!(
            "this worker was removed from the cluster, so it stopped. To add it again, {JOIN_AGAIN}"
        ))),
        Exit::Expired => Err(Failure::Failed(format!(
            "this worker's certificate expired (it was offline too long to renew it), so it \
             stopped. To add it again, {JOIN_AGAIN}"
        ))),
        Exit::Outdated { coordinator_major } => Err(Failure::Failed(format!(
            "the coordinator runs cluster protocol {coordinator_major}, which this version \
             doesn't speak. Update Lodestar on this machine to the coordinator's version."
        ))),
        Exit::IdentityMismatch => Err(Failure::Failed(
            "the coordinator didn't accept this worker's identity. Join again.".to_owned(),
        )),
        Exit::Replaced => Err(Failure::Failed(
            "another worker with these credentials connected, so this one stopped. Run one \
             copy per data folder, and don't copy a joined folder to another machine."
                .to_owned(),
        )),
    }
}

fn block_on(f: impl Future<Output = Outcome>) -> Outcome {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(failed("starting the runtime"))?
        .block_on(f)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command};

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("lodestar").chain(args.iter().copied()))
    }

    #[test]
    fn the_token_comes_only_from_a_file_or_stdin() {
        let cli = parse(&["worker", "--join", "https://c:8091", "--token-file", "-"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Worker(WorkerArgs {
                join: Some("https://c:8091".into()),
                token_file: Some("-".into()),
                data: None,
                rotate_key: false,
            }))
        );
        // No flag takes the token itself.
        for flag in ["--token", "--join-token", "--secret"] {
            assert!(parse(&["worker", flag, "lsj1x"]).is_err(), "{flag}");
        }
        // A token file needs an address, and a rotation joins nothing.
        // (`--join` alone is for LODESTAR_TOKEN, checked in `plan`.)
        assert!(parse(&["worker", "--join", "https://c"]).is_ok());
        assert!(parse(&["worker", "--token-file", "t"]).is_err());
        assert!(parse(&["worker", "--rotate-key", "--join", "c", "--token-file", "t"]).is_err());
        assert!(parse(&["worker", "--data", "/w", "--rotate-key"]).is_ok());
    }

    fn args(join: Option<&str>, token_file: Option<&str>) -> WorkerArgs {
        WorkerArgs {
            join: join.map(Into::into),
            token_file: token_file.map(Into::into),
            data: None,
            rotate_key: false,
        }
    }

    #[test]
    fn lodestar_token_is_for_the_first_join_only() {
        let token = "lsj1secret".to_owned();
        let env = |k: &str| match k {
            "LODESTAR_TOKEN" => Some(token.clone()),
            "LODESTAR_JOIN" => Some("https://c:8091".to_owned()),
            _ => None,
        };
        let none = |_: &str| None;
        // Not joined: the variable joins, at LODESTAR_JOIN or --join.
        assert_eq!(
            plan(&args(None, None), &env, false).unwrap(),
            Plan {
                join: Some(("https://c:8091".into(), TokenSource::Env(token.clone()))),
                warnings: vec![],
            }
        );
        let only_token = |k: &str| (k == "LODESTAR_TOKEN").then(|| token.clone());
        assert_eq!(
            plan(&args(Some("https://d"), None), &only_token, false)
                .unwrap()
                .join,
            Some(("https://d".into(), TokenSource::Env(token.clone())))
        );
        let Err(Failure::Failed(m)) = plan(&args(None, None), &only_token, false) else {
            panic!("no address");
        };
        assert!(m.contains("LODESTAR_JOIN") && !m.contains(&token), "{m}");
        // Joined: ignored, with the warning, and nothing joins.
        assert_eq!(
            plan(&args(None, None), &env, true).unwrap(),
            Plan {
                join: None,
                warnings: vec![ENV_TOKEN_STALE],
            }
        );
        // A token file wins, and says the variable is ignored.
        assert_eq!(
            plan(&args(Some("https://c"), Some("t")), &env, true).unwrap(),
            Plan {
                join: Some(("https://c".into(), TokenSource::File("t".into()))),
                warnings: vec![ENV_TOKEN_UNUSED],
            }
        );
        // A blank variable is unset, and --join alone needs a token.
        let blank = |k: &str| (k == "LODESTAR_TOKEN").then(|| " ".to_owned());
        assert_eq!(plan(&args(None, None), &blank, false).unwrap().join, None);
        assert!(plan(&args(Some("https://c"), None), &none, false).is_err());
        for w in [ENV_TOKEN_STALE, ENV_TOKEN_UNUSED] {
            assert!(!w.contains(&token));
        }
    }

    #[test]
    fn a_plan_never_prints_the_token() {
        let env = |k: &str| (k == "LODESTAR_TOKEN").then(|| "lsj1secretvalue".to_owned());
        let plan = plan(&args(Some("https://c"), None), &env, false).unwrap();
        let shown = format!("{plan:?}");
        assert!(
            !shown.contains("secretvalue") && shown.contains("Env(***)"),
            "{shown}"
        );
        // Joined: both the variable and --join are said to be ignored.
        let joined = plan_warnings(&args(Some("https://c"), None), &env);
        assert_eq!(joined, vec![ENV_TOKEN_STALE, JOIN_IGNORED]);
        assert!(!ENV_TOKEN_STALE.contains("  "), "{ENV_TOKEN_STALE}");
    }

    fn plan_warnings(a: &WorkerArgs, env: &dyn Fn(&str) -> Option<String>) -> Vec<&'static str> {
        plan(a, env, true).unwrap().warnings
    }

    #[test]
    fn data_is_the_configs_folder_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[cluster]\nenabled = true\n",
        )
        .unwrap();
        let worker = Command::Worker(WorkerArgs {
            join: None,
            token_file: None,
            data: Some(dir.path().to_path_buf()),
            rotate_key: false,
        });
        let config = crate::cli::load_config(&worker, None, |_| None).unwrap();
        // The folder, and the config.toml read from it.
        assert_eq!(config.data_dir, dir.path());
        assert!(config.cluster.enabled);
        // Other commands keep LODESTAR_DATA_DIR.
        let other = crate::cli::load_config(&Command::Vacuum, None, |_| None).unwrap();
        assert_eq!(other.data_dir, std::path::PathBuf::from("data"));
    }

    #[test]
    fn a_restarted_image_still_warns() {
        let env = |k: &str| (k == TOKEN_IGNORED).then(|| "1".to_owned());
        assert_eq!(
            plan_warnings(&args(None, None), &env),
            vec![ENV_TOKEN_STALE_RESTARTED]
        );
        // With a token file: that one wins, and the variable is said to be
        // ignored.
        let unused = plan(&args(Some("https://c"), Some("t")), &env, false).unwrap();
        assert_eq!(unused.warnings, vec![ENV_TOKEN_UNUSED_RESTARTED]);
        // Set by hand, with no token: each line also says to remove the
        // marker, so neither is wrong (WK-2's review).
        for w in [ENV_TOKEN_STALE_RESTARTED, ENV_TOKEN_UNUSED_RESTARTED] {
            assert!(w.contains("remove LODESTAR_TOKEN_IGNORED"), "{w}");
            assert!(!w.contains("  "), "{w}");
        }
        // Not joined (the folder changed meanwhile): nothing to warn about.
        assert_eq!(
            plan(&args(None, None), &env, false).unwrap().warnings,
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_bad_variable_token_fails_without_showing_it() {
        let text = "lsj1notatokenatall".to_owned();
        let e = parse_and_wipe(text.clone()).unwrap_err().to_string();
        assert!(!e.contains(&text) && !e.contains("notatoken"), "{e}");
    }

    #[test]
    fn rotation_is_refused_while_the_worker_runs() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(None, |_| None).unwrap();
        let layout = Layout::new(dir.path());
        let held = layout.lock().unwrap().expect("free");
        let rotate = || WorkerArgs {
            join: None,
            token_file: None,
            data: Some(dir.path().to_path_buf()),
            rotate_key: true,
        };
        assert!(matches!(
            run_with(&config, rotate(), &|_| None),
            Err(Failure::Refused(_))
        ));
        assert!(!dir.path().join("secrets.key").exists(), "nothing rotated");
        drop(held);
        assert!(run_with(&config, rotate(), &|_| None).is_ok());
        assert!(dir.path().join("secrets.key").exists());
    }

    #[test]
    fn every_stop_is_one_line_that_says_what_to_do() {
        let report: Report = Arc::new(|_| {});
        for exit in [Exit::Revoked, Exit::Expired] {
            let Err(Failure::Failed(m)) = finish(exit, &report) else {
                panic!("{exit:?}");
            };
            assert!(m.contains("--join") && !m.contains('\n'), "{m}");
        }
        assert!(finish(Exit::Drained, &report).is_ok());
        assert!(finish(Exit::Shutdown, &report).is_ok());
    }

    #[test]
    fn a_coordinators_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lodestar.db"), b"").unwrap();
        let config = Config::load(None, |_| None).unwrap();
        let args = WorkerArgs {
            join: None,
            token_file: None,
            data: Some(dir.path().to_path_buf()),
            rotate_key: true,
        };
        assert!(matches!(run(&config, args), Err(Failure::Refused(_))));
        assert!(!dir.path().join("secrets.key").exists());
    }
}
