//! Lodestar as a Windows service (architecture §11).
//!
//! The installer (P-release) registers the service as [`NAME`] with the
//! image path `"<install dir>\lodestar.exe"`, optionally followed by
//! `--config <path>`. No subcommand, `serve`, or plain `worker` (a worker
//! of an existing Lodestar, WK-18s) marks a service start: when the
//! arguments are such a command line and the process has no console,
//! `main` asks the Service Control Manager whether it started it
//! ([`run_if_launched`]), and an ordinary run is told no at once.
//!
//! Started by the SCM, Lodestar:
//! - first makes sure `%ProgramData%\Lodestar` is private
//!   ([`super::acl`]), and refuses to start if it isn't: the reason, with
//!   the exact fix, goes beside the installer's log, and the exit code is
//!   3, which the SCM is given as `0x20000003` ([`scm_code`]);
//! - keeps its data there unless `LODESTAR_DATA_DIR` or the config file
//!   says otherwise, and runs from that folder, so relative paths in the
//!   config land there;
//! - logs to `<data>\logs\lodestar.log` ([`super::logfile`]), where the
//!   first run's setup token appears;
//! - maps Stop, Shutdown and Preshutdown to the same graceful shutdown as
//!   Ctrl+C, acknowledging at once (also while the store opens) and
//!   reporting progress so the SCM waits for it;
//! - reports a service-specific exit code 1 (`0x20000001`) when it stops
//!   on an error, so the SCM's recovery actions (restart) apply;
//! - on Restart now, takes the same graceful shutdown as a stop, then
//!   reports [`RESTART`] (`0x20000050`), so the recovery action starts it
//!   again after 60 s. It's offered only while [`restarts`], Lodestar's own
//!   count of recoveries used in the last 24 hours, is empty: the SCM
//!   restarts a service twice a day at most, so a crash always has one
//!   left.

use std::path::{Path, PathBuf};

use super::acl;
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

/// CR-0d §1.5: with the restart proof on and no witness dir chosen (in the
/// config file or `LODESTAR_DHT_WITNESS_DIR`), the service keeps its
/// witness in `%ProgramData%\Lodestar-witness`: beside the data folder, so
/// a backup of the data folder doesn't hold it, and not in the virtual
/// account's profile. The service makes it private like the data folder;
/// with the switch off nothing is created. An empty variable chooses
/// nothing, as `Config::load` reads it. `env` reads the environment.
pub fn default_witness_dir(
    config: &Config,
    env: &impl Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let chosen = config.layers.in_file("dht.restart_witness_dir")
        || env("LODESTAR_DHT_WITNESS_DIR").is_some_and(|v| !v.is_empty());
    (config.dht.safe_restart && !chosen).then(|| {
        PathBuf::from(env("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into()))
            .join("Lodestar-witness")
    })
}

/// Gives the service its [`default_witness_dir`] when that applies: made
/// private with [`acl::secure_dir`], never weaker. When it can't be used
/// the run has no witness (`no_witness`, safe: every restart suppresses),
/// not the native default in the service account's profile, and this
/// returns the folder and why, for a Warn event. A data folder set to
/// ProgramData or the witness path would back the witness up with the
/// data, so that counts as unusable too.
pub fn apply_witness_default(
    config: &mut Config,
    env: &impl Fn(&str) -> Option<String>,
    own: Option<&str>,
    ops: &impl acl::Ops,
) -> Option<(PathBuf, &'static str)> {
    let w = default_witness_dir(config, env)?;
    config.dht.restart_witness_dir = None;
    if within(&w, &config.data_dir) {
        return Some((w, "inside_data"));
    }
    match acl::secure_dir(&w, own, ops) {
        Ok(()) => {
            config.dht.restart_witness_dir = Some(w);
            None
        }
        // secure_dir's message names the path: its step only.
        Err(why) if why.starts_with("creating ") => Some((w, "create")),
        Err(why) if why.starts_with("reading ") => Some((w, "inspect")),
        Err(_) => Some((w, "not_private")),
    }
}

/// The service's config and data folder, with its own folder `default_dir`
/// made private by `secure` before anything in it is read, by the same rule
/// the CLI uses ([`crate::cli::ServiceGuard`]), so the two never disagree:
/// - before any config is read: when neither `--config` (`explicit`) nor
///   `LODESTAR_DATA_DIR` points elsewhere (`config.toml` there is read),
///   when `LODESTAR_DATA_DIR` names that folder or a folder inside it, or
///   when the config file is inside it;
/// - after loading, before anything else: when the data folder is that
///   folder or inside it.
///
/// `secure` runs at most once. An error is the Event Log's [`Refusal`]
/// and the full reason: why the folder is used and isn't private, or which
/// setting isn't a full path. A config that failed to load has
/// `default_dir` as its data folder, for the log.
///
/// No path setting is resolved against the working folder, which is the
/// system folder under the SCM (X-win-service-relative-paths and -2): an
/// empty or relative `LODESTAR_DATA_DIR`, a relative `--config`, a relative
/// `default_dir` (from `%ProgramData%`), and a relative `data_dir`,
/// restart witness folder (`dht.restart_witness_dir` or
/// `LODESTAR_DHT_WITNESS_DIR`) or
/// `blocklist.files` entry is refused, naming the setting. The first three
/// are refused before anything is read, the rest once the config is loaded
/// and before any of them is used. The CLI keeps relative paths, which a
/// portable install uses.
///
/// The SCM path calls [`load_checked`], which also gives the folder the
/// Event Log names; this keeps the earlier shape for the tests.
// Outside the tests nothing calls it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn load_guarded(
    default_dir: &Path,
    explicit: Option<PathBuf>,
    env: impl Fn(&str) -> Option<String>,
    secure: impl FnOnce() -> Result<(), String>,
) -> Result<(Result<Config, String>, PathBuf), (Refusal, String)> {
    load_checked(default_dir, explicit, env, secure).map_err(|(r, why, _)| (r, why))
}

/// [`load_checked`]'s answer: the config and data folder, or the refusal,
/// its full reason and the folder the Event Log names.
pub type Checked = Result<(Result<Config, String>, PathBuf), (Refusal, String, PathBuf)>;

/// [`load_guarded`], with the folder the Event Log names: the unusable
/// data folder for [`Refusal::Unusable`], else `default_dir`.
///
/// X-win-data-dir-unusable-log: a data folder that is a file, can't be
/// read or can't be created is refused ([`unusable`]), naming the setting
/// that chose it. The service's own folder is checked just before the ACL
/// step, which would otherwise fail first and point at Repair, and never
/// created there ([`acl::secure_dir`] creates it private). Any other data
/// folder is checked, and created if missing, once the config is loaded
/// and the guard has run, before anything uses it.
pub fn load_checked(
    default_dir: &Path,
    explicit: Option<PathBuf>,
    env: impl Fn(&str) -> Option<String>,
    secure: impl FnOnce() -> Result<(), String>,
) -> Checked {
    // LODESTAR_DATA_DIR naming the service's folder, or a folder inside
    // it, is what led there.
    let by_env = env("LODESTAR_DATA_DIR")
        .is_some_and(|d| Path::new(&d).is_absolute() && within(Path::new(&d), default_dir));
    let found = std::cell::Cell::new(None);
    let checked = || match unusable(default_dir, false) {
        Some(problem) => {
            found.set(Some(problem));
            Err(String::new())
        }
        None => secure(),
    };
    let (config, data_dir, file) =
        load_paths(default_dir, explicit, env, checked).map_err(|(r, why)| match found.take() {
            Some((problem, detail)) => {
                let setting = if by_env {
                    Setting::Env
                } else {
                    Setting::Default
                };
                unusable_refusal(setting, default_dir, problem, &detail, None)
            }
            None => (r, why, default_dir.to_path_buf()),
        })?;
    let setting = match &config {
        Ok(c) if c.layers.in_env("LODESTAR_DATA_DIR") => Setting::Env,
        Ok(c) if c.layers.in_file("data_dir") => Setting::File,
        _ => Setting::Default,
    };
    if let Some((problem, detail)) = unusable(&data_dir, true) {
        return Err(unusable_refusal(
            setting,
            &data_dir,
            problem,
            &detail,
            file.as_deref(),
        ));
    }
    Ok((config, data_dir))
}

/// [`load_guarded`]'s load and checks, with the config file it read.
#[allow(clippy::type_complexity)]
fn load_paths(
    default_dir: &Path,
    explicit: Option<PathBuf>,
    env: impl Fn(&str) -> Option<String>,
    secure: impl FnOnce() -> Result<(), String>,
) -> Result<(Result<Config, String>, PathBuf, Option<PathBuf>), (Refusal, String)> {
    use crate::cli::util::Failure;
    use crate::cli::{LoadError, ServiceGuard};
    let why = |e: LoadError| match e {
        LoadError::Folder(Failure::Refused(m) | Failure::Failed(m)) => (Refusal::NotPrivate, m),
        LoadError::Config(e) => (Refusal::NotPrivate, format!("config: {e}")),
    };
    if !default_dir.is_absolute() {
        return Err(not_full(
            &format!(
                "the service's default data folder is {:?}, from %ProgramData%, which isn't a \
                 full path",
                default_dir.display().to_string()
            ),
            "set the system's ProgramData variable back to a full path, such as C:\\ProgramData",
        ));
    }
    let data_env = env("LODESTAR_DATA_DIR");
    match data_env.as_deref() {
        Some("") => {
            return Err(not_full(
                "LODESTAR_DATA_DIR is set for the service but empty",
                "set it to a full path, such as D:\\Lodestar, or remove it",
            ));
        }
        Some(d) if !Path::new(d).is_absolute() => {
            return Err(not_full(
                &format!("LODESTAR_DATA_DIR for the service is {d:?}, which isn't a full path"),
                "set it to a full path, such as D:\\Lodestar",
            ));
        }
        _ => {}
    }
    if let Some(p) = explicit.as_deref().filter(|p| !p.is_absolute()) {
        return Err(not_full(
            &format!(
                "--config in the service's command line is {:?}, which isn't a full path",
                p.display().to_string()
            ),
            "give --config a full path, such as D:\\Lodestar\\config.toml",
        ));
    }
    let elsewhere = explicit.is_some() || data_env.is_some();
    let mut guard = ServiceGuard::for_dir(default_dir.to_path_buf(), |_: &Path| {
        secure().map_err(Failure::Refused)
    });
    guard
        .before(explicit.as_deref(), data_env.as_deref(), !elsewhere)
        .map_err(why)?;
    let path = config_path(explicit, default_dir, &env);
    let loaded = match (&path, &data_env, default_dir.to_str()) {
        // No config.toml in its folder and no LODESTAR_DATA_DIR: loaded as if
        // pointed at its folder (X-win-service-cwd-fallback), as the CLI's
        // MSI branch does, so `data\config.toml` in the working folder (the
        // system folder under the SCM) is never read. The folder isn't then
        // counted as set by the environment.
        (None, None, Some(d)) => Config::load(None, |k| match k {
            "LODESTAR_DATA_DIR" => Some(d.to_owned()),
            _ => env(k),
        })
        .map(|mut c| {
            c.layers.env.retain(|v| *v != "LODESTAR_DATA_DIR");
            c
        }),
        _ => Config::load(path.as_deref(), &env),
    };
    let config = loaded
        .map(|mut c| {
            default_data_dir_unless_set(&mut c, default_dir);
            c
        })
        .map_err(|e| format!("config: {e}"));
    let data_dir = config
        .as_ref()
        .map_or(default_dir.to_path_buf(), |c| c.data_dir.clone());
    // Only the file can still name a relative folder: LODESTAR_DATA_DIR was
    // checked above, and the default is `default_dir`.
    if !data_dir.is_absolute() {
        let file = path
            .as_deref()
            .map_or_else(String::new, |p| p.display().to_string());
        return Err(not_full(
            &format!(
                "data_dir in the service's config file {file:?} is {:?}, which isn't a full path",
                data_dir.display().to_string()
            ),
            "set data_dir to a full path, such as D:/Lodestar",
        ));
    }
    if let Some(refused) = config
        .as_ref()
        .ok()
        .and_then(|c| relative_path_setting(c, path.as_deref(), &env))
    {
        return Err(refused);
    }
    guard.after(&data_dir).map_err(why)?;
    Ok((config, data_dir, path))
}

/// The service's refusal of the config's other path settings when one
/// isn't a full path (X-win-service-relative-paths-2): the restart witness
/// folder, whichever way it was chosen, and each blocklist file. Checked
/// after loading, before any of them is created, opened or made private.
fn relative_path_setting(
    c: &Config,
    file: Option<&Path>,
    env: &impl Fn(&str) -> Option<String>,
) -> Option<(Refusal, String)> {
    let file = file.map_or_else(String::new, |p| p.display().to_string());
    let shown = |p: &Path| p.display().to_string();
    // Config::load: the variable over the file. Chosen neither way, the
    // service never uses the loader's native default: with the restart
    // proof on, [`apply_witness_default`] replaces it.
    let chosen = if env("LODESTAR_DHT_WITNESS_DIR").is_some_and(|v| !v.is_empty()) {
        Some("LODESTAR_DHT_WITNESS_DIR for the service is".to_owned())
    } else if c.layers.in_file("dht.restart_witness_dir") {
        Some(format!(
            "dht.restart_witness_dir in the service's config file {file:?} is"
        ))
    } else {
        None
    };
    let relative = c
        .dht
        .restart_witness_dir
        .as_deref()
        .filter(|w| !w.is_absolute());
    if let (Some(what), Some(w)) = (chosen, relative) {
        return Some(not_full(
            &format!("{what} {:?}, which isn't a full path", shown(w)),
            "set it to a full path, such as D:/Lodestar-witness, or remove it",
        ));
    }
    c.blocklist
        .files
        .iter()
        .position(|f| !f.is_absolute())
        .map(|i| {
            not_full(
                &format!(
                    "blocklist.files entry {} in the service's config file {file:?} is {:?}, \
                     which isn't a full path",
                    i + 1,
                    shown(&c.blocklist.files[i])
                ),
                "give every blocklist file a full path, such as D:/lists/hashes.txt",
            )
        })
}

/// A refusal of a setting that isn't a full path: what's wrong, why, and
/// the fix, for the temp log.
fn not_full(what: &str, fix: &str) -> (Refusal, String) {
    (
        Refusal::NotFullPath,
        format!(
            "{what}. The service never resolves a path against its working folder, which is \
             the system folder when Windows starts it, so it didn't start: {fix}."
        ),
    )
}

/// The setting that chose an unusable data folder ([`Refusal::Unusable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Nothing chose one: the service's own folder, under `%ProgramData%`.
    Default,
    /// `LODESTAR_DATA_DIR`.
    Env,
    /// `data_dir` in the config file.
    File,
}

impl Setting {
    /// Where the folder came from, for both logs.
    const fn source(self) -> &'static str {
        match self {
            Setting::Default => "the default, under %ProgramData%",
            Setting::Env => "from LODESTAR_DATA_DIR",
            Setting::File => "from data_dir in its config file",
        }
    }
}

/// What's wrong with an unusable data folder ([`Refusal::Unusable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// It's a file, not a folder.
    File,
    /// The service's account can't list it.
    Unreadable,
    /// It's missing and can't be made: a file or a missing drive is in the
    /// way, or creating it failed.
    Uncreatable,
}

impl Problem {
    const fn text(self) -> &'static str {
        match self {
            Problem::File => "is a file, not a folder",
            Problem::Unreadable => "can't be read by the service account",
            Problem::Uncreatable => "can't be created",
        }
    }
}

/// The fix for an unusable data folder, in plain words, after "an
/// administrator should".
fn unusable_fix(setting: Setting, problem: Problem) -> String {
    let name = match setting {
        Setting::Default => {
            return match problem {
                Problem::File => {
                    "move or rename that file, then start the service again, which creates \
                     the folder"
                }
                Problem::Unreadable => "run the Lodestar installer's Repair",
                Problem::Uncreatable => {
                    "make sure %ProgramData% is a folder on a working drive, then run the \
                     Lodestar installer's Repair"
                }
            }
            .to_owned();
        }
        Setting::Env => "LODESTAR_DATA_DIR",
        Setting::File => "data_dir",
    };
    match problem {
        Problem::File => format!("set {name} to a folder, or move that file away"),
        Problem::Unreadable => format!(
            "give the Lodestar service full control of that folder, or set {name} to a folder \
             it can use"
        ),
        Problem::Uncreatable => {
            format!("create that folder, or set {name} to a folder on a drive that exists")
        }
    }
}

/// The refusal of an unusable data folder `dir`: the Event Log's kind,
/// the temp log's full reason (`detail` is the system's own error, if
/// any), and the folder itself, for the Event Log.
fn unusable_refusal(
    setting: Setting,
    dir: &Path,
    problem: Problem,
    detail: &str,
    file: Option<&Path>,
) -> (Refusal, String, PathBuf) {
    let file = match (setting, file) {
        (Setting::File, Some(f)) => format!(" {:?}", f.display().to_string()),
        _ => String::new(),
    };
    let detail = match detail {
        "" => String::new(),
        d => format!(" ({d})"),
    };
    (
        Refusal::Unusable(setting, problem),
        format!(
            "the service's data folder {:?}, {}{file}, {}{detail}, so the service didn't start. \
             To fix it, an administrator should {}.",
            dir.display().to_string(),
            setting.source(),
            problem.text(),
            unusable_fix(setting, problem),
        ),
        dir.to_path_buf(),
    )
}

/// Why the data folder `dir` can't be used, with the system's error, or
/// `None` when it can. Nothing is followed or created through a link or
/// junction, or any other reparse point: with one on the way, this says
/// nothing, and the folder is used as before ([`acl::secure_dir`] refuses
/// them in the service's own folder). A missing folder is created when
/// `create` is set and the nearest folder that exists is a real one.
fn unusable(dir: &Path, create: bool) -> Option<(Problem, String)> {
    use std::io::ErrorKind::{NotADirectory, NotFound};
    let meta = |p: &Path| std::fs::symlink_metadata(p);
    let link = |p: &Path| meta(p).is_ok_and(|m| is_link(&m));
    match meta(dir) {
        Ok(m) if is_link(&m) => None,
        Ok(m) if !m.is_dir() => Some((Problem::File, String::new())),
        Ok(_) => std::fs::read_dir(dir)
            .err()
            .map(|e| (Problem::Unreadable, e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Some((Problem::Unreadable, e.to_string()))
        }
        // An invalid name or an unreachable network path, say.
        Err(e) if !matches!(e.kind(), NotFound | NotADirectory) => {
            Some((Problem::Uncreatable, e.to_string()))
        }
        Err(_) => {
            for (i, up) in dir.ancestors().enumerate().skip(1) {
                match meta(up) {
                    Ok(m) if is_link(&m) => return None,
                    Ok(m) if !m.is_dir() => {
                        return Some((Problem::Uncreatable, format!("{} is a file", up.display())));
                    }
                    Ok(_) if !create || dir.ancestors().skip(i + 1).any(link) => return None,
                    Ok(_) => {
                        return std::fs::create_dir_all(dir)
                            .err()
                            .map(|e| (Problem::Uncreatable, e.to_string()));
                    }
                    Err(e) if matches!(e.kind(), NotFound | NotADirectory) => {}
                    Err(e) => {
                        return Some((Problem::Uncreatable, format!("{}: {e}", up.display())));
                    }
                }
            }
            Some((Problem::Uncreatable, "no part of its path exists".into()))
        }
    }
}

/// A link, junction or any other reparse point.
fn is_link(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}

/// Why the service won't open the store: an update rollback is in
/// progress (the update helper's marker, `<data>\bin\rollback.json`).
/// The SCM path refuses as `serve` and every CLI subcommand do (§12.2).
/// The marker only ever stops a start; nothing runs from it.
pub fn rollback_refusal(data_dir: &Path) -> Option<String> {
    crate::cli::util::refuse_during_update_rollback(data_dir)
        .err()
        .map(|e| e.to_string())
}

/// The Windows Application event log source the service writes under.
/// The update helper writes as "Lodestar update helper"
/// (`update/windows/helper.rs`). The MSI registers both as `eventcreate`
/// sources (`packaging/windows/main.wxs`), so the messages show without
/// "The description for Event ID … cannot be found", and the service's
/// account, which may not create a source, can still write.
pub const EVENT_SOURCE: &str = "Lodestar";

/// Why the service didn't start normally, as the Event Log tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The data folder isn't private, or its permissions couldn't be read
    /// or set ([`super::acl::secure_dir`]): the start is refused.
    NotPrivate,
    /// An update rollback holds the store: the service waits, idle.
    RollingBack,
    /// A path setting isn't a full path: `LODESTAR_DATA_DIR`, `--config`,
    /// `%ProgramData%`, `data_dir`, the restart witness folder or a
    /// `blocklist.files` entry ([`load_guarded`]): the start is refused.
    NotFullPath,
    /// The data folder, chosen by the [`Setting`], has the [`Problem`]
    /// ([`load_checked`]): the start is refused.
    Unusable(Setting, Problem),
}

impl Refusal {
    /// `eventcreate`'s `/T` and `/ID`: within 1–1000, which its own
    /// message file renders as the text alone. 3 is the refused start's
    /// exit code (Windows service error 3).
    pub const fn event(self) -> (&'static str, u16) {
        match self {
            Refusal::NotPrivate => ("ERROR", 3),
            Refusal::RollingBack => ("WARNING", 4),
            Refusal::NotFullPath => ("ERROR", 5),
            Refusal::Unusable(..) => ("ERROR", 6),
        }
    }
}

/// What the service writes to the Windows Application event log when it
/// refuses to start, or waits for a rollback. Every local user can read
/// that log, and the Legal Notice (section 5) allows only what happened,
/// how to fix it, and folder paths. So this is a fixed sentence, the
/// folder `dir`, and for [`Refusal::Unusable`] the setting's name alone:
/// never [`super::acl`]'s own reason, which names accounts and SIDs, nor
/// anything else from the config. The full reason, with the exact command,
/// goes to `%SystemRoot%\Temp\lodestar-service.log` only.
pub fn event_log_text(refusal: Refusal, dir: &Path) -> String {
    // One line, no control or invisible characters: a folder name can't
    // forge a second message.
    let dir = one_line(&dir.display().to_string());
    // A configured folder can be long; `eventcreate` takes a short line.
    let dir = match dir.char_indices().nth(MAX_SHOWN) {
        Some((at, _)) => format!("{}…", &dir[..at]),
        None => dir,
    };
    let log = super::temp_log::NAME;
    match refusal {
        Refusal::NotPrivate => format!(
            "Lodestar's service didn't start: its data folder, {dir}, isn't private, or it \
             couldn't be checked or created, so Lodestar didn't use it. To fix it, \
             run the Lodestar installer's Repair as an administrator. If Repair stops or the \
             service still won't start, the exact reason and fix are in \
             %SystemRoot%\\Temp\\{log}; see Fix Windows service error 3 in the user guide."
        ),
        Refusal::RollingBack => format!(
            "Lodestar's service is waiting: an update is being undone in its data folder, {dir}, \
             and nothing may open the store until that finishes. The service carries on by itself \
             once it has. If it keeps waiting, the reason and what to do are in \
             %SystemRoot%\\Temp\\{log}."
        ),
        Refusal::NotFullPath => format!(
            "Lodestar's service didn't start: a path setting, such as LODESTAR_DATA_DIR, \
             --config or a folder or file in its config file, isn't a full path, and the service never uses a path relative to the \
             system folder. Its default data folder is {dir}. To fix it, an administrator sets \
             that setting to a full path, such as D:\\Lodestar, or removes it. The temp log, \
             %SystemRoot%\\Temp\\{log}, names the setting; see Fix Windows service error 3 in the user \
             guide."
        ),
        Refusal::Unusable(setting, problem) => format!(
            "Lodestar's service didn't start: its data folder, {dir} ({}), {}. To fix it, an \
             administrator should {}. The reason is in %SystemRoot%\\Temp\\{log}; see Fix \
             Windows service error 3 in the user guide.",
            setting.source(),
            problem.text(),
            unusable_fix(setting, problem),
        ),
    }
}

/// `text` on one line: control characters become spaces, and Unicode's
/// format and separator characters (bidirectional controls, zero-width and
/// soft-hyphen characters, tags), which could make a name read as other
/// text, are dropped (`acl`'s list).
pub fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !super::acl::invisible(*c))
        .collect()
}

/// The most of a folder's name the Event Log message shows, in characters.
const MAX_SHOWN: usize = 260;

/// Runs the refusal note's `eventcreate` (`cmd`) for at most `limit`,
/// through the shared bounded run (`windows::process`): a hung one is
/// killed and reaped, and the refusal goes on whatever happened, since the
/// temp log and the exit code already carry it. The service has reported
/// Running by then, so without the bound a hung `eventcreate` would keep
/// it Running, and on the rollback path the stop signal would never be
/// seen (rv-702 N2; X-svc-eventcreate-bounded).
fn write_event_note(
    cmd: std::process::Command,
    limit: std::time::Duration,
) -> std::io::Result<i32> {
    super::process::run_bounded(cmd, "eventcreate", limit)
}

/// The refusal note's `eventcreate`, started as the update helper starts
/// its own: from `System32` by absolute path with no console window
/// ([`super::system32_command`]), and with every `LODESTAR_*` variable
/// removed ([`crate::update::windows::helper::clean_env`]), so both writers
/// spawn the same way (rv-728 N4). `None` without `%SystemRoot%`.
#[cfg(windows)]
fn event_note_command(kind: &str, id: u16, text: &str) -> Option<std::process::Command> {
    let mut cmd = super::system32_command("eventcreate.exe")?;
    crate::update::windows::helper::clean_env(&mut cmd);
    let id = id.to_string();
    cmd.args(["/T", kind, "/ID", id.as_str(), "/L", "APPLICATION"])
        .args(["/SO", EVENT_SOURCE, "/D"])
        .arg(text);
    Some(cmd)
}

/// How the service's idle wait during a rollback ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Idle {
    /// The SCM asked it to stop (or shut down).
    Stopped,
    /// The marker went: the rollback finished, or an administrator
    /// removed it. The service carries on into its normal start in the
    /// same process (a fresh config load and the same checks), so one the
    /// MSI started while the marker existed is running afterwards.
    MarkerGone,
}

impl Idle {
    /// The exit code to end with, or `None` to carry on into the start.
    pub fn exit_code(&self) -> Option<u32> {
        match self {
            Idle::Stopped => Some(0),
            Idle::MarkerGone => None,
        }
    }
}

/// How often the idle service looks for the marker and a stop.
pub const IDLE_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// While the rollback marker exists the service stays Running and idle:
/// it never opens the store and serves nothing, until `stopped()` (exit
/// 0) or the marker goes (on into the normal start). Exiting at once
/// instead would fail an MSI's `ServiceControl Start="install"
/// Wait="yes"` (error 1920, exit 1603): the reinstall of the previous
/// version during a rollback, or an administrator's manual one, would
/// fail on the marker.
pub fn idle_during_rollback(
    data_dir: &Path,
    stopped: &dyn Fn() -> bool,
    poll: std::time::Duration,
) -> Idle {
    loop {
        if stopped() {
            return Idle::Stopped;
        }
        if rollback_refusal(data_dir).is_none() {
            return Idle::MarkerGone;
        }
        std::thread::sleep(poll);
    }
}

/// Whether `inner` is `outer` or inside it, compared as [`same_dir`] does,
/// by whole path parts: `inner` or one of its folders is `outer` by what
/// the system says it is ([`object`]), or by its text.
pub fn within(inner: &Path, outer: &Path) -> bool {
    if let Some(o) = object(outer)
        && full(inner)
            .ancestors()
            .any(|a| object(a).as_ref() == Some(&o))
    {
        return true;
    }
    let (i, o) = (norm(inner), norm(outer));
    i == o || i.starts_with(&format!("{o}\\"))
}

/// What the system says an existing file or folder is: its volume serial
/// number and file index, opened with `FILE_FLAG_BACKUP_SEMANTICS`
/// (`same_file::Handle`), after links and junctions. `None` when it can't
/// be opened (missing, or not readable by this account), and always off
/// Windows: the caller then compares the text.
#[cfg(windows)]
fn object(p: &Path) -> Option<same_file::Handle> {
    same_file::Handle::from_path(full(p)).ok()
}

/// Off Windows nothing is compared by file identity.
#[cfg(not(windows))]
fn object(_: &Path) -> Option<std::convert::Infallible> {
    None
}

/// Whether two paths are the same file or folder by [`object`]; `None` when
/// either can't be opened.
#[cfg(windows)]
fn same_object(a: &Path, b: &Path) -> Option<bool> {
    Some(object(a)? == object(b)?)
}

/// Off Windows nothing is compared by file identity.
#[cfg(not(windows))]
fn same_object(_: &Path, _: &Path) -> Option<bool> {
    None
}

/// The text compared when [`object`] can't decide. The part of the path
/// that exists is the system's own real path, compared as it is: only the
/// verbatim prefix goes, and case. The rest, not created yet, is read as
/// Win32 reads it ([`tidy`]), unless the path was given verbatim (`\\?\`):
/// Win32 takes a verbatim path literally, so no part of it is tidied.
fn norm(p: &Path) -> String {
    let p = full(p);
    let literal = p.to_string_lossy().replace('/', "\\").starts_with(r"\\?\");
    let s = match real(&p) {
        Some((canonical, rest)) => {
            let head = verbatim(&canonical);
            if rest.is_empty() {
                head
            } else if literal {
                let tail: Vec<String> = rest
                    .iter()
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect();
                format!("{head}\\{}", tail.join("\\"))
            } else {
                let protect = head.split('\\').filter(|c| !c.is_empty()).count();
                let tail: Vec<String> = rest
                    .iter()
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect();
                tidy(&format!("{head}\\{}", tail.join("\\")), protect)
            }
        }
        None if literal => verbatim(&p),
        None => tidy(&p.to_string_lossy().replace('/', "\\"), 0),
    };
    s.to_lowercase()
}

/// A path's text as given, `/` as `\`, without a verbatim `\\?\` prefix
/// (`\\?\UNC\` back to `\\`) or a trailing separator: nothing else changes.
fn verbatim(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = match s.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => rest.to_string(),
        },
        None => s,
    };
    s.trim_end_matches('\\').to_string()
}

/// The full path as Win32 makes it before opening anything
/// (`GetFullPathNameW`, through `std::path::absolute`): relative to the
/// working folder, with `.` and `..`, trailing dots and spaces, and `/`
/// resolved. Elsewhere the path as given.
fn full(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
    }
    #[cfg(not(windows))]
    {
        p.to_path_buf()
    }
}

/// A path's text (`\` separators) read as Win32 reads one it resolves:
/// `.` and empty components go, `..` takes the one before it (never the
/// drive or a UNC server and share), and trailing dots and spaces go from
/// each name. The first `protect` names are the system's own and stay as
/// they are, though `..` may still take them.
fn tidy(s: &str, protect: usize) -> String {
    let (lead, body) = match s.strip_prefix(r"\\") {
        Some(b) => (r"\\", b),
        None => match s.strip_prefix('\\') {
            Some(b) => ("\\", b),
            None => ("", s),
        },
    };
    let mut out: Vec<&str> = Vec::new();
    for (i, c) in body.split('\\').filter(|c| !c.is_empty()).enumerate() {
        if i < protect {
            out.push(c);
            continue;
        }
        if c == ".." {
            let keep = match (lead, out.first()) {
                (r"\\", _) => 2,
                (_, Some(first)) if first.ends_with(':') => 1,
                _ => 0,
            };
            if out.len() > keep {
                out.pop();
            }
            continue;
        }
        let c = c.trim_end_matches(['.', ' ']);
        if !c.is_empty() {
            out.push(c);
        }
    }
    format!("{lead}{}", out.join("\\"))
}

/// `p`'s nearest existing folder's real path, and the names after it not
/// created yet (outermost first): a folder not created yet (the witness)
/// then compares with one that exists (the data folder), whatever 8.3
/// names or links lead there. `None` when no folder on the way resolves.
fn real(p: &Path) -> Option<(PathBuf, Vec<std::ffi::OsString>)> {
    let mut rest = Vec::new();
    let mut at = p;
    loop {
        if let Ok(r) = std::fs::canonicalize(at) {
            rest.reverse();
            return Some((r, rest));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return None,
        }
    }
}

/// Whether two paths name the same folder, as Windows compares them: by
/// what the system says each is when both can be opened ([`same_object`]),
/// else by their text ([`norm`]): the real path of what exists, as it is
/// (so case, 8.3 names, links and `..` don't matter), and the rest as Win32
/// reads it ([`full`], [`tidy`]), case-insensitively, without a `\\?\`
/// prefix or a trailing separator. A path given verbatim (`\\?\`) is never
/// tidied: Win32 takes it literally.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    same_object(a, b) == Some(true) || norm(a) == norm(b)
}

/// What a service command line runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// `serve` (or no subcommand): Lodestar itself.
    Serve,
    /// Plain `worker`: a worker of an existing Lodestar, from the
    /// credentials the installer's one-shot `--join-only` saved (WK-18s).
    Worker,
}

/// A service command line: its role and its optional `--config <path>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCommand {
    pub config: Option<PathBuf>,
    pub role: Role,
}

/// The service command line the arguments are (none, `serve` or plain
/// `worker`, and an optional `--config <path>`), else `None`: every other
/// command runs as usual, without asking the SCM anything.
pub fn service_command<I, T>(args: I) -> Option<ServiceCommand>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use crate::cli::Command;
    use clap::Parser;
    let cli = crate::cli::Cli::try_parse_from(args).ok()?;
    let role = match cli.command {
        // `serve --fixture` serves test data offline: never a service start.
        None
        | Some(Command::Serve {
            fixture: None,
            child: false,
        }) => Role::Serve,
        // Only with no flags: a service never joins (a token file would join
        // again on every start), never rotates and keeps the default folder,
        // which the start-up check secures.
        Some(Command::Worker(crate::cli::worker::WorkerArgs {
            join: None,
            token_file: None,
            data: None,
            rotate_key: false,
            join_only: false,
        })) => Role::Worker,
        _ => return None,
    };
    Some(ServiceCommand {
        config: cli.config,
        role,
    })
}

#[cfg(windows)]
pub use scm::run_if_launched;

/// The Windows convention for an application's own error codes (the
/// "customer" bit). Without it, the SCM's event 7024 looks a
/// service-specific code up in Windows' own messages, so a refusal (3)
/// read "The system cannot find the path specified" and an error (1)
/// "Incorrect function". With it nothing is looked up: the event shows the
/// number, and the reason is in `%SystemRoot%\Temp\lodestar-service.log`.
pub const CUSTOMER: u32 = 0x2000_0000;

/// The service-specific code the SCM is given for an exit code: 0 stays 0,
/// anything else gets [`CUSTOMER`] (3 becomes 536870915, `0x20000003`).
pub const fn scm_code(code: u32) -> u32 {
    if code == 0 { 0 } else { code | CUSTOMER }
}

/// The exit code for Restart now: the service stopped cleanly so that the
/// SCM's recovery action starts it again. Distinct from 1 (an error) and 3
/// (a refused start); the same number as the Unix supervisor's
/// `EXIT_RESTART`. The SCM sees [`scm_code`]`(80)`, `0x20000050`.
pub const RESTART: u32 = 80;

/// What `/system` says while Restart now is paused.
pub const PAUSED_HINT: &str = "Restart now is paused for about a day: Lodestar restarted or \
     stopped unexpectedly recently, and Windows restarts the service automatically only twice a \
     day, so one is kept for a crash. To restart now, restart the Lodestar service in Windows \
     Services (services.msc).";

/// Lodestar's own count of the SCM recoveries used in the last 24 hours
/// (plan Q10). The SCM doesn't expose its failure count, and the MSI lets
/// it restart the service twice, then never, until a day without failures
/// has passed (`packaging/windows/main.wxs`). Every Restart now uses one,
/// and so may any start after an unclean stop, so each is recorded here,
/// and Restart now is offered only while none is in the window.
///
/// The file is untrusted input: bounded, parsed strictly, and anything
/// missing (with a store already there), unreadable, oversized, a link or
/// malformed counts as "a recovery may have been used", which pauses
/// Restart now (fails closed). It's written beside a temporary file and
/// renamed into place, never through a link, in the private data folder.
///
/// ```text
/// lodestar-service-restarts 1
/// state running
/// restart 1762560000
/// unclean_stop 1762570000
/// ```
pub mod restarts {
    use std::io::{Read as _, Write as _};
    use std::path::{Path, PathBuf};

    /// In the data folder.
    pub const FILE: &str = "service-restarts.txt";
    pub const MAX_BYTES: u64 = 4096;
    /// How long a recovery counts: the MSI's `ResetPeriodInDays="1"`.
    pub const WINDOW_SECS: u64 = 86_400;
    /// Added to the window, so the count is strictly conservative: an
    /// entry is stamped just before the exit, a moment before the SCM
    /// records the failure, and Windows doesn't say which clock its reset
    /// period follows.
    pub const MARGIN_SECS: u64 = 3_600;
    /// How long an entry counts.
    pub const COUNTS_SECS: u64 = WINDOW_SECS + MARGIN_SECS;
    const HEADER: &str = "lodestar-service-restarts 1";
    /// More can't matter: one already pauses Restart now.
    const MAX_ENTRIES: usize = 32;

    /// Why a recovery may have been used.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        /// Restart now.
        Restart,
        /// A start after a run that didn't record a clean stop (a crash,
        /// an error exit, a kill).
        UncleanStop,
        /// The record was missing or unreadable.
        Unverified,
    }

    impl Kind {
        /// The word in the file, and the event's `reason`.
        pub const fn word(self) -> &'static str {
            match self {
                Kind::Restart => "restart",
                Kind::UncleanStop => "unclean_stop",
                Kind::Unverified => "unverified",
            }
        }

        fn parse(word: &str) -> Option<Kind> {
            [Kind::Restart, Kind::UncleanStop, Kind::Unverified]
                .into_iter()
                .find(|k| k.word() == word)
        }
    }

    /// What reading the file found.
    #[derive(Debug, PartialEq, Eq)]
    pub enum Found {
        Missing,
        Text(String),
        /// Unreadable, oversized, not a plain file, or not UTF-8.
        Bad,
    }

    /// The record: whether a run is in progress, and the recoveries.
    #[derive(Debug, Default, Clone, PartialEq, Eq)]
    pub struct Ledger {
        pub running: bool,
        /// Oldest first, Unix seconds.
        pub entries: Vec<(Kind, u64)>,
    }

    impl Ledger {
        /// The record as this start leaves it: entries over a day old
        /// dropped, an unclean stop or an unverified record added, and a run
        /// in progress. `store_exists` is whether the store was there before
        /// this start: a missing file then means a fresh install, not a
        /// deleted record.
        pub fn at_start(found: Found, store_exists: bool, now: u64) -> Ledger {
            let mut ledger = match found {
                Found::Missing if !store_exists => Ledger::default(),
                Found::Text(text) => match parse(&text) {
                    Some(mut l) => {
                        if l.running {
                            l.entries.push((Kind::UncleanStop, now));
                        }
                        l
                    }
                    None => Ledger::unverified(now),
                },
                Found::Missing | Found::Bad => Ledger::unverified(now),
            };
            ledger.prune(now);
            ledger.running = true;
            ledger
        }

        fn unverified(now: u64) -> Ledger {
            Ledger {
                running: false,
                entries: vec![(Kind::Unverified, now)],
            }
        }

        /// Drops entries over [`COUNTS_SECS`] old. One from the future (the
        /// clock went back) counts from now, so it can't linger.
        fn prune(&mut self, now: u64) {
            for (_, at) in &mut self.entries {
                *at = (*at).min(now);
            }
            self.entries.retain(|&(_, at)| now - at < COUNTS_SECS);
            self.entries.sort_by_key(|&(_, at)| at);
            let extra = self.entries.len().saturating_sub(MAX_ENTRIES);
            self.entries.drain(..extra);
        }

        /// Why Restart now is paused: the newest recovery's kind, if any.
        pub fn paused(&self) -> Option<Kind> {
            self.entries.last().map(|&(k, _)| k)
        }

        /// When the newest recovery leaves the window, and Restart now is
        /// offered again.
        pub fn paused_until(&self) -> Option<u64> {
            self.entries
                .last()
                .map(|&(_, at)| at.saturating_add(COUNTS_SECS))
        }

        /// A clean stop.
        pub fn stopped(&mut self) {
            self.running = false;
        }

        /// A clean stop for Restart now, which uses a recovery.
        pub fn restarted(&mut self, now: u64) {
            self.running = false;
            self.entries.push((Kind::Restart, now));
            self.prune(now);
        }

        pub fn render(&self) -> String {
            let mut out = format!(
                "{HEADER}\nstate {}\n",
                if self.running { "running" } else { "stopped" }
            );
            for (kind, at) in &self.entries {
                out.push_str(&format!("{} {at}\n", kind.word()));
            }
            out
        }
    }

    /// The record in `text`, or `None` if anything about it is off.
    pub fn parse(text: &str) -> Option<Ledger> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let running = match lines.next()? {
            "state running" => true,
            "state stopped" => false,
            _ => return None,
        };
        let mut entries = Vec::new();
        for line in lines {
            let (word, at) = line.split_once(' ')?;
            // Digits only: no sign, space or `+`.
            if at.is_empty() || at.len() > 20 || !at.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            entries.push((Kind::parse(word)?, at.parse().ok()?));
            if entries.len() > MAX_ENTRIES {
                return None;
            }
        }
        Some(Ledger { running, entries })
    }

    pub fn path(dir: &Path) -> PathBuf {
        dir.join(FILE)
    }

    /// Opens without following a final link: Windows opens a reparse
    /// point itself (which is then refused), Unix fails with `ELOOP`.
    fn no_follow(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            /// `FILE_FLAG_OPEN_REPARSE_POINT`.
            const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(OPEN_REPARSE_POINT)
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        }
        #[cfg(not(any(windows, unix)))]
        {
            options
        }
    }

    /// Whether `meta` is a plain file and no link.
    fn plain(meta: &std::fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            /// `FILE_ATTRIBUTE_REPARSE_POINT`.
            const REPARSE_POINT: u32 = 0x400;
            meta.is_file() && meta.file_attributes() & REPARSE_POINT == 0
        }
        #[cfg(not(windows))]
        {
            meta.is_file()
        }
    }

    pub fn read(dir: &Path) -> Found {
        let file = match no_follow(std::fs::OpenOptions::new().read(true)).open(path(dir)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Found::Missing,
            Err(_) => return Found::Bad,
        };
        match file.metadata() {
            Ok(meta) if plain(&meta) && meta.len() <= MAX_BYTES => {}
            _ => return Found::Bad,
        }
        let mut bytes = Vec::new();
        if file.take(MAX_BYTES + 1).read_to_end(&mut bytes).is_err()
            || bytes.len() as u64 > MAX_BYTES
        {
            return Found::Bad;
        }
        String::from_utf8(bytes).map_or(Found::Bad, Found::Text)
    }

    /// Replaces the record: a new temporary file (a stale one, or a link
    /// planted in its place, is removed first, not followed), synced, then
    /// renamed over the old record, which replaces a link rather than
    /// writing through it.
    pub fn write(dir: &Path, ledger: &Ledger) -> std::io::Result<()> {
        let tmp = dir.join(format!("{FILE}.tmp"));
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut file =
            no_follow(std::fs::OpenOptions::new().write(true).create_new(true)).open(&tmp)?;
        file.write_all(ledger.render().as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path(dir))
    }

    /// Whether Restart now is paused: why, and until when (`None`: for
    /// the whole run).
    pub type Pause = Option<(&'static str, Option<u64>)>;

    /// At a service start, before the store opens: records this start and
    /// says whether Restart now is paused, why and until when. A record
    /// that can't be written pauses it for the whole run (`unwritable`):
    /// the next start couldn't count this run's recovery.
    pub fn begin(dir: &Path, store_exists: bool, now: u64) -> (Ledger, Pause) {
        let ledger = Ledger::at_start(read(dir), store_exists, now);
        let paused = match write(dir, &ledger) {
            Ok(()) => ledger.paused().map(|k| (k.word(), ledger.paused_until())),
            Err(e) => {
                lodestar_log::log_event!(
                    crate::logging::events::SERVICE_RESTARTS_UNWRITABLE,
                    error = lodestar_log::Error::io(&e)
                );
                Some(("unwritable", None))
            }
        };
        (ledger, paused)
    }

    /// At the end of a run that stopped cleanly; `restart` for Restart now.
    pub fn end(dir: &Path, ledger: &mut Ledger, restart: bool, now: u64) {
        if restart {
            ledger.restarted(now);
        } else {
            ledger.stopped();
        }
        if let Err(e) = write(dir, ledger) {
            // The record still says running, so the next start counts an
            // unclean stop: a recovery is still counted.
            lodestar_log::log_event!(
                crate::logging::events::SERVICE_RESTARTS_UNWRITABLE,
                error = lodestar_log::Error::io(&e)
            );
        }
    }
}

#[cfg(windows)]
mod scm {
    use std::ffi::OsString;
    use std::io::IsTerminal;
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use tokio::sync::watch;
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
    /// What the command line runs, for `service_main`.
    static ROLE: OnceLock<super::Role> = OnceLock::new();
    /// The service's exit code, for the process's.
    static EXIT: OnceLock<u32> = OnceLock::new();
    /// For the control handler, which runs on the dispatcher's thread.
    static STATUS: OnceLock<ServiceStatusHandle> = OnceLock::new();
    static CHECKPOINT: AtomicU32 = AtomicU32::new(1);

    define_windows_service!(ffi_service_main, service_main);

    /// Runs Lodestar as a service if the SCM started this process with
    /// `config` from its command line, and returns its exit code once it
    /// has stopped; `None` for an ordinary run.
    pub fn run_if_launched(command: super::ServiceCommand) -> Option<ExitCode> {
        // A service has no console; a console run needn't ask the SCM.
        if std::io::stdin().is_terminal() {
            return None;
        }
        let _ = CONFIG_PATH.set(command.config);
        let _ = ROLE.set(command.role);
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
                // Acknowledged at once, even while the store is opening,
                // and kept ticking from a thread of its own, which a long
                // blocking open can't starve.
                if let Some(status) = STATUS.get() {
                    stop_pending(*status);
                    tick_while_stopping(*status);
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
        // The folder's ACL names the service's own SID; without it, the
        // check refuses the folder.
        let own = acl::service_sid(name).ok();
        // A loop only for the rollback marker: once it goes, the same
        // process starts normally, from a fresh load.
        let code = loop {
            break match load(own.as_deref()) {
                // Refused before anything was read or written in the default
                // folder, so the reason goes outside it, beside the
                // installer's log; the exit code (3) says it was refused.
                // The Event Log names the default folder, or the unusable
                // data folder itself.
                Err((refusal, why, dir)) => {
                    log_refusal(&why);
                    event_log(refusal, &dir);
                    REFUSED
                }
                // A rollback holds the store: neither version opens it until
                // the helper (or an administrator) has finished. The service
                // stays Running and idle meanwhile, so an MSI that starts it
                // and waits succeeds. Stopped, it ends cleanly (0, no failure
                // actions); once the marker goes, it starts normally.
                Ok((_, data_dir, _)) if super::rollback_refusal(&data_dir).is_some() => {
                    log_refusal(&super::rollback_refusal(&data_dir).unwrap_or_default());
                    event_log(super::Refusal::RollingBack, &data_dir);
                    let idle = super::idle_during_rollback(
                        &data_dir,
                        &|| *stop_rx.borrow(),
                        super::IDLE_POLL,
                    );
                    match idle.exit_code() {
                        Some(code) => code,
                        // The marker went: on into the normal start, so a
                        // service the MSI started while the marker existed is
                        // running when the rollback's step 5 finds it.
                        None => continue,
                    }
                }
                Ok((config, data_dir, witness_refused)) => {
                    let _ = std::env::set_current_dir(&data_dir);
                    init_logging(
                        &data_dir,
                        config.as_ref().map_or("info", |c| c.log.as_str()),
                    );
                    lodestar_log::log_event!(
                        crate::logging::events::SERVICE_STARTING,
                        path = lodestar_log::Path::new(&data_dir)
                    );
                    if let Some((w, why)) = &witness_refused {
                        lodestar_log::log_event!(
                            crate::logging::events::SERVICE_WITNESS_REFUSED,
                            path = lodestar_log::Path::new(w),
                            error = lodestar_log::Error::kind(why)
                        );
                    }
                    let role = ROLE.get().copied().unwrap_or(super::Role::Serve);
                    match config.map_err(BoxError::from).and_then(|c| match role {
                        super::Role::Serve => serve(&c, stop_rx),
                        super::Role::Worker => worker(&c, stop_rx),
                    }) {
                        Ok(Ended::Stopped) => {
                            lodestar_log::log_event!(crate::logging::events::SERVICE_STOPPED);
                            0
                        }
                        Ok(Ended::Restart) => {
                            lodestar_log::log_event!(
                                crate::logging::events::SERVICE_RESTARTING,
                                code = u64::from(super::scm_code(super::RESTART))
                            );
                            super::RESTART
                        }
                        // Its message can name a path: the kind only, or the
                        // store's WAL refusal by name (X-store-wal-nits).
                        Err(e) => {
                            let kind = match role {
                                super::Role::Serve => "serve",
                                super::Role::Worker => "worker",
                            };
                            lodestar_log::log_event!(
                                crate::logging::events::SERVICE_FAILED,
                                error = lodestar_log::Error::kind(crate::failure_kind(&*e, kind))
                            );
                            1
                        }
                    }
                }
            };
        };
        report(
            status,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            code,
        );
        code
    }

    /// The config and the data folder, with `%ProgramData%\Lodestar` made
    /// private ([`acl`]) before anything in it is read, whenever it's used
    /// ([`super::load_checked`]). An error is the Event Log's refusal, the
    /// full reason and the folder to name. The third part is the restart
    /// witness folder when it can't be used, and why.
    #[allow(clippy::type_complexity)]
    fn load(
        own: Option<&str>,
    ) -> Result<
        (
            Result<Config, String>,
            PathBuf,
            Option<(PathBuf, &'static str)>,
        ),
        (super::Refusal, String, PathBuf),
    > {
        let default_dir = super::default_data_dir(std::env::var("ProgramData").ok());
        let env = |k: &str| std::env::var(k).ok();
        let explicit = CONFIG_PATH.get().cloned().flatten();
        let (mut config, data_dir) = super::load_checked(&default_dir, explicit, env, || {
            acl::secure_dir(&default_dir, own, &acl::Windows { own_sid: own })
        })?;
        let witness_refused = config.as_mut().ok().and_then(|c| {
            super::apply_witness_default(c, &env, own, &acl::Windows { own_sid: own })
        });
        Ok((config, data_dir, witness_refused))
    }

    /// Appends a refusal's reason to `%SystemRoot%\Temp\lodestar-service.log`,
    /// where the installer logs too, never through a planted link
    /// ([`super::super::temp_log`]). Only [`acl::secure_dir`]'s refusals
    /// (paths, SIDs, account names and the fix), a setting that isn't a full
    /// path (its name, value and the fix), an unusable data folder (its
    /// path, setting, the system's error and the fix) and the update helper's rollback
    /// marker come here; the SCM records the exit code ([`REFUSED`])
    /// in the System event log, and [`event_log`] a summary without the
    /// accounts in the Application event log.
    fn log_refusal(why: &str) {
        let _ = crate::windows::temp_log::append("service refused to start", why);
    }

    /// Writes [`super::event_log_text`] to the Windows Application event
    /// log as [`super::EVENT_SOURCE`], through `eventcreate` started as the
    /// update helper starts its own ([`super::event_note_command`]; the
    /// workspace forbids `unsafe`, so no `ReportEventW`). Best effort, and bounded
    /// ([`super::write_event_note`]): the temp log and the exit code
    /// already say it. The Legal Notice's section 5 names this write.
    fn event_log(refusal: super::Refusal, dir: &std::path::Path) {
        let (kind, id) = refusal.event();
        if let Some(cmd) = super::event_note_command(kind, id, &super::event_log_text(refusal, dir))
        {
            let _ = super::write_event_note(cmd, crate::windows::process::EVENT_LOG_WAIT);
        }
    }

    /// Serialises status reports, so a progress tick never follows
    /// Stopped; `true` once Stopped is reported.
    static REPORTS: Mutex<bool> = Mutex::new(false);
    static TICKING: AtomicBool = AtomicBool::new(false);

    fn set_status(status: ServiceStatusHandle, next: ServiceStatus) {
        let mut stopped = REPORTS.lock().unwrap_or_else(|e| e.into_inner());
        if *stopped {
            return;
        }
        *stopped = next.current_state == ServiceState::Stopped;
        let _ = status.set_service_status(next);
    }

    fn report(
        status: ServiceStatusHandle,
        state: ServiceState,
        accept: ServiceControlAccept,
        code: u32,
    ) {
        set_status(
            status,
            ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: accept,
                exit_code: if code == 0 {
                    ServiceExitCode::NO_ERROR
                } else {
                    ServiceExitCode::ServiceSpecific(super::scm_code(code))
                },
                checkpoint: 0,
                wait_hint: Duration::ZERO,
                process_id: None,
            },
        );
    }

    /// Reports StopPending with the next checkpoint.
    fn stop_pending(status: ServiceStatusHandle) {
        set_status(
            status,
            ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::StopPending,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code: ServiceExitCode::NO_ERROR,
                checkpoint: CHECKPOINT.fetch_add(1, Ordering::Relaxed),
                wait_hint: STOP_WAIT_HINT,
                process_id: None,
            },
        );
    }

    /// Reports progress every [`STOP_TICK`] until Stopped (once per process).
    fn tick_while_stopping(status: ServiceStatusHandle) {
        if TICKING.swap(true, Ordering::Relaxed) {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("service-stopping".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(STOP_TICK);
                    if *REPORTS.lock().unwrap_or_else(|e| e.into_inner()) {
                        return;
                    }
                    stop_pending(status);
                }
            });
    }

    /// How a run that didn't fail ended.
    enum Ended {
        Stopped,
        /// Restart now: exit with [`super::RESTART`] for the SCM.
        Restart,
    }

    fn now() -> u64 {
        crate::unix_now().max(0) as u64
    }

    fn serve(config: &Config, stop: watch::Receiver<bool>) -> Result<Ended, BoxError> {
        super::super::startup(config);
        let restart = crate::system::Restart::global();
        restart.set_service();
        // Recorded before the store opens, so a start that fails or dies
        // there leaves `running` and the next start counts it. Before
        // `Services::start` creates the store, a missing record is a fresh
        // install's, not a deleted one. The folder is created as
        // `Services::start` would.
        let store_existed = config.db_path().exists();
        std::fs::create_dir_all(&config.data_dir)?;
        let (mut ledger, paused) = super::restarts::begin(&config.data_dir, store_existed, now());
        if let Some((reason, until)) = paused {
            lodestar_log::log_event!(
                crate::logging::events::SERVICE_RESTART_PAUSED,
                reason = reason
            );
            match until {
                Some(until) => restart.pause_until(until, super::PAUSED_HINT),
                // Unwritable: for this whole run.
                None => restart.refuse(super::PAUSED_HINT),
            }
        }
        // Read again after the runtime has shut down: a stop then still
        // wins, so the SCM never restarts a service someone stopped.
        let after = stop.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let served: Result<bool, BoxError> = runtime.block_on(async {
            let mut early = stop.clone();
            // A stop while the store opens ends the start: the open's
            // blocking work may be cut off after `RUNTIME_GRACE` as the
            // runtime shuts down. It's crash-safe (each migration is one
            // transaction, and runs again at the next start).
            let services = tokio::select! {
                s = Services::start(config) => s?,
                () = stopped(&mut early) => return Ok(false),
            };
            let mut stop = stop;
            services
                .run(async move {
                    tokio::select! {
                        () = stopped(&mut stop) => {}
                        () = restart.requested() => {}
                    }
                })
                .await?;
            Ok(restart.was_requested())
        });
        // A blocking task still running can't keep the SCM waiting.
        runtime.shutdown_timeout(crate::services::RUNTIME_GRACE);
        // An error leaves the record at `running`: the next start counts it.
        let restarting = served? && !*after.borrow();
        super::restarts::end(&config.data_dir, &mut ledger, restarting, now());
        Ok(if restarting {
            Ended::Restart
        } else {
            Ended::Stopped
        })
    }

    /// The worker (WK-18s): `lodestar worker` from the saved credentials,
    /// stopped gracefully by the SCM's Stop or Shutdown. It has no store,
    /// web page or Restart now; an error is reported as for `serve` (code 1),
    /// so the SCM's recovery restarts it. Its progress lines have no log
    /// event yet, so only the service's own events are logged.
    fn worker(config: &Config, stop: watch::Receiver<bool>) -> Result<Ended, BoxError> {
        std::fs::create_dir_all(&config.data_dir)?;
        let args = crate::cli::worker::WorkerArgs {
            join: None,
            token_file: None,
            data: None,
            rotate_key: false,
            join_only: false,
        };
        let mut stop = stop;
        let stopping = async move { stopped(&mut stop).await };
        let report: crate::worker::session::Report = std::sync::Arc::new(|_: &str| {});
        crate::cli::worker::run_until(config, args, stopping, report)
            .map(|()| Ended::Stopped)
            .map_err(|e| {
                // Why it stopped (revoked, expired, outdated, …), which says
                // what to do and never holds a token, goes beside the
                // installer's log, as a refused start's reason does.
                let _ = crate::windows::temp_log::append(
                    "worker stopped",
                    &super::one_line(&e.to_string()),
                );
                BoxError::from(e.to_string())
            })
    }

    /// Resolves once a stop is requested.
    async fn stopped(stop: &mut watch::Receiver<bool>) {
        while !*stop.borrow_and_update() {
            if stop.changed().await.is_err() {
                // The handler is gone; nothing can stop us later.
                std::future::pending::<()>().await;
            }
        }
        lodestar_log::log_event!(crate::logging::events::SERVICE_STOPPING);
    }

    /// The service's logging: the same runtime as everywhere else
    /// (`settings::log::init_with_writer`), its console going to
    /// `<data>\logs\lodestar.log` (where the setup token's line is found)
    /// and its JSONL file opening through `follow` in the logs folder.
    /// Without the file, the console goes nowhere; the JSONL file still
    /// opens.
    fn init_logging(data_dir: &std::path::Path, filter: &str) {
        let console: Box<dyn std::io::Write + Send> =
            match LogFile::open(logfile::path(data_dir), logfile::MAX_BYTES) {
                Ok(file) => Box::new(logfile::Console::new(std::sync::Arc::new(file))),
                Err(_) => Box::new(std::io::sink()),
            };
        crate::settings::log::init_with_writer(filter, console);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `--config` path of a serve command line.
    fn command(s: &str) -> Option<Option<PathBuf>> {
        let c = role_of(s)?;
        assert_eq!(c.role, Role::Serve, "{s}");
        Some(c.config)
    }

    fn role_of(s: &str) -> Option<ServiceCommand> {
        service_command(std::iter::once("lodestar").chain(s.split_whitespace()))
    }

    /// X-win-service-path-hardening (d): the service makes its own folder
    /// private before it reads any config from there, however the folder
    /// is named, and checks a data folder inside it too (b). Each case
    /// tells the two orders apart: a check that repairs the file before
    /// it's read, or a file inside the folder that moves the data away.
    #[test]
    fn the_service_secures_its_folder_before_reading_from_it() {
        let root = tempfile::tempdir().unwrap();
        let svc = root.path().join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        std::fs::write(svc.join("config.toml"), "not toml [").unwrap();
        let toml = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let env_of = |pairs: Vec<(&'static str, String)>| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
        };
        let refuse = || Err::<(), _>("isn't private".to_string());
        let refused = |r: Result<(Result<Config, String>, PathBuf), (Refusal, String)>| matches!(r, Err((Refusal::NotPrivate, ref why)) if why == "isn't private");

        // Nothing set: its config.toml is read, so checked first, even when
        // that file moves the data elsewhere (read first, the folder would
        // never be checked).
        assert!(refused(load_guarded(&svc, None, env_of(vec![]), refuse)));
        let away_data = root.path().join("F").join("Lodestar");
        std::fs::write(
            svc.join("config.toml"),
            format!("data_dir = \"{}\"\n", toml(&away_data)),
        )
        .unwrap();
        assert!(refused(load_guarded(&svc, None, env_of(vec![]), refuse)));
        std::fs::write(svc.join("config.toml"), "not toml [").unwrap();
        // LODESTAR_DATA_DIR naming it, or a folder inside it: the check runs
        // before its config.toml is read. The check here makes the broken
        // file valid, so the config loads only if it was read after.
        for d in [svc.clone(), svc.join("data")] {
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("config.toml"), "not toml [").unwrap();
            let env = env_of(vec![(
                "LODESTAR_DATA_DIR",
                d.to_string_lossy().into_owned(),
            )]);
            let repair = || std::fs::write(d.join("config.toml"), "").map_err(|e| e.to_string());
            let (c, _) = load_guarded(&svc, None, env, repair).unwrap();
            assert!(c.is_ok(), "{d:?}: read before the check: {c:?}");
        }
        std::fs::write(svc.join("config.toml"), "not toml [").unwrap();
        // `--config` inside it, moving the data elsewhere: refused before
        // it's read. Read first, the data would be elsewhere and the folder
        // never checked.
        let elsewhere = root.path().join("E").join("Lodestar");
        let moving = svc.join("moving.toml");
        std::fs::write(&moving, format!("data_dir = \"{}\"\n", toml(&elsewhere))).unwrap();
        assert!(refused(load_guarded(
            &svc,
            Some(moving),
            env_of(vec![]),
            refuse
        )));
        // `--config` elsewhere naming a data folder inside it: checked after
        // loading, before anything else.
        let to_inside = root.path().join("inside.toml");
        std::fs::write(
            &to_inside,
            format!("data_dir = \"{}\"\n", toml(&svc.join("data"))),
        )
        .unwrap();
        assert!(refused(load_guarded(
            &svc,
            Some(to_inside),
            env_of(vec![]),
            refuse
        )));

        // A sibling whose name starts the same: never checked.
        let sibling = root.path().join("ProgramData").join("Lodestar2");
        let env = env_of(vec![(
            "LODESTAR_DATA_DIR",
            sibling.to_string_lossy().into_owned(),
        )]);
        let unasked_sibling = || -> Result<(), String> { panic!("sibling checked") };
        let (_, d) = load_guarded(&svc, None, env, unasked_sibling).unwrap();
        assert_eq!(d, sibling);

        // Another folder: never checked.
        let away = root.path().join("away.toml");
        let other = root.path().join("D").join("Lodestar");
        std::fs::write(&away, format!("data_dir = \"{}\"\n", toml(&other))).unwrap();
        let unasked = || -> Result<(), String> { panic!("checked") };
        let (c, d) = load_guarded(&svc, Some(away), env_of(vec![]), unasked).unwrap();
        assert_eq!((c.unwrap().data_dir, d), (other.clone(), other));

        // Private: read once, after one check.
        std::fs::write(svc.join("config.toml"), "").unwrap();
        let calls = std::cell::Cell::new(0);
        let once = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let (c, d) = load_guarded(&svc, None, env_of(vec![]), once).unwrap();
        assert_eq!((c.unwrap().data_dir, d, calls.get()), (svc.clone(), svc, 1));
    }

    /// X-win-service-relative-paths: an empty or relative
    /// `LODESTAR_DATA_DIR`, a relative `--config` and a relative `data_dir`
    /// in the config file are refused, naming the setting, and the first
    /// two before anything is read or checked. Full paths load as before.
    #[test]
    fn the_service_refuses_a_setting_that_isnt_a_full_path() {
        let root = tempfile::tempdir().unwrap();
        let svc = root.path().join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        let env_of =
            |v: &'static str| move |k: &str| (k == "LODESTAR_DATA_DIR").then(|| v.to_owned());
        let unasked = || -> Result<(), String> { panic!("checked before refusing") };
        let refused = |r: Result<(Result<Config, String>, PathBuf), (Refusal, String)>,
                       name: &str| match r {
            Err((Refusal::NotFullPath, why)) => {
                assert!(why.contains(name) && why.contains("full path"), "{why}");
                why
            }
            other => panic!("{name}: not refused: {other:?}"),
        };

        let why = refused(
            load_guarded(&svc, None, env_of(""), unasked),
            "LODESTAR_DATA_DIR",
        );
        assert!(why.contains("empty"), "{why}");
        let mut relative = vec!["data", "./data", "../Lodestar"];
        // Relative on Windows only: to the current drive's root, or to a
        // drive's own working folder.
        if cfg!(windows) {
            relative.extend([r"\Lodestar", "D:Lodestar", "C:"]);
        }
        for d in relative {
            let env = move |k: &str| (k == "LODESTAR_DATA_DIR").then(|| d.to_owned());
            let why = refused(load_guarded(&svc, None, env, unasked), "LODESTAR_DATA_DIR");
            assert!(why.contains(d), "{why}");
            let why = refused(
                load_guarded(
                    &svc,
                    Some(PathBuf::from(d).join("config.toml")),
                    |_| None,
                    unasked,
                ),
                "--config",
            );
            assert!(why.contains("config.toml"), "{why}");
        }

        // A config file with a relative data_dir: refused once it's read,
        // naming the file.
        let file = root.path().join("relative.toml");
        std::fs::write(&file, "data_dir = \"data\"\n").unwrap();
        let why = refused(
            load_guarded(&svc, Some(file.clone()), |_| None, || Ok(())),
            "data_dir",
        );
        assert!(why.contains("relative.toml"), "{why}");
        std::fs::write(svc.join("config.toml"), "data_dir = \"\"\n").unwrap();
        refused(load_guarded(&svc, None, |_| None, || Ok(())), "data_dir");
        std::fs::remove_file(svc.join("config.toml")).unwrap();

        // A relative service folder (an empty or relative %ProgramData%):
        // refused, naming it.
        refused(
            load_guarded(Path::new("Lodestar"), None, |_| None, unasked),
            "ProgramData",
        );

        // Full paths load as before.
        let away = root.path().join("away");
        let shown = away.to_string_lossy().into_owned();
        let env = move |k: &str| (k == "LODESTAR_DATA_DIR").then(|| shown.clone());
        let (c, d) = load_guarded(&svc, None, env, || Ok(())).unwrap();
        assert_eq!((c.unwrap().data_dir, d), (away.clone(), away.clone()));
        std::fs::write(&file, format!("data_dir = {:?}\n", away.to_string_lossy())).unwrap();
        let (c, d) = load_guarded(&svc, Some(file), |_| None, || Ok(())).unwrap();
        assert_eq!((c.unwrap().data_dir, d), (away.clone(), away));
        let (c, d) = load_guarded(&svc, None, |_| None, || Ok(())).unwrap();
        assert_eq!((c.unwrap().data_dir, d), (svc.clone(), svc));
    }

    /// X-win-service-relative-paths-2: a relative restart witness folder
    /// (from the file or `LODESTAR_DHT_WITNESS_DIR`) or `blocklist.files`
    /// entry is refused, naming the setting, before anything uses it; full
    /// paths load as before.
    #[test]
    fn the_service_refuses_other_relative_path_settings() {
        let root = tempfile::tempdir().unwrap();
        let svc = root.path().join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        let file = root.path().join("service.toml");
        let toml = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let data = format!("data_dir = \"{}\"\n", toml(&root.path().join("data")));
        let full = toml(&root.path().join("full"));
        let refused = |r: Result<(Result<Config, String>, PathBuf), (Refusal, String)>,
                       name: &str,
                       value: &str| match r {
            Err((Refusal::NotFullPath, why)) => {
                assert!(why.contains(name), "{name}: {why}");
                assert!(why.contains(value), "{value}: {why}");
                assert!(why.contains("full path"), "{why}");
            }
            other => panic!("{name}={value}: not refused: {other:?}"),
        };
        let mut relative = vec!["w", "./w", "../w"];
        // Relative on Windows only: to the current drive's root, or to a
        // drive's own working folder. Off Windows these are plain names.
        if cfg!(windows) {
            relative.extend([r"\w", "C:w"]);
        }
        for v in relative {
            let q = v.replace('\\', "\\\\");
            // In the file.
            std::fs::write(
                &file,
                format!("{data}[dht]\nrestart_witness_dir = \"{q}\"\n"),
            )
            .unwrap();
            refused(
                load_guarded(&svc, Some(file.clone()), |_| None, || Ok(())),
                "dht.restart_witness_dir",
                &q,
            );
            // From the environment, over a full path in the file.
            std::fs::write(
                &file,
                format!("{data}[dht]\nrestart_witness_dir = \"{full}\"\n"),
            )
            .unwrap();
            let env = move |k: &str| (k == "LODESTAR_DHT_WITNESS_DIR").then(|| v.to_owned());
            refused(
                load_guarded(&svc, Some(file.clone()), env, || Ok(())),
                "LODESTAR_DHT_WITNESS_DIR",
                &q,
            );
            // A blocklist file, after a full one: named by its place.
            std::fs::write(
                &file,
                format!("{data}[blocklist]\nfiles = [\"{full}\", \"{q}\"]\n"),
            )
            .unwrap();
            refused(
                load_guarded(&svc, Some(file.clone()), |_| None, || Ok(())),
                "blocklist.files entry 2",
                &q,
            );
        }

        // Full paths load as before, and an empty variable chooses nothing.
        std::fs::write(
            &file,
            format!(
                "{data}[dht]\nrestart_witness_dir = \"{full}\"\n[blocklist]\nfiles = [\"{full}/a.txt\"]\n"
            ),
        )
        .unwrap();
        let empty = |k: &str| (k == "LODESTAR_DHT_WITNESS_DIR").then(String::new);
        let (c, _) = load_guarded(&svc, Some(file.clone()), empty, || Ok(())).unwrap();
        let c = c.unwrap();
        assert_eq!(c.dht.restart_witness_dir, Some(PathBuf::from(&full)));
        assert_eq!(
            c.blocklist.files,
            vec![PathBuf::from(format!("{full}/a.txt"))]
        );
        let shown = full.clone();
        let env = move |k: &str| (k == "LODESTAR_DHT_WITNESS_DIR").then(|| shown.clone());
        std::fs::write(&file, &data).unwrap();
        let (c, _) = load_guarded(&svc, Some(file), env, || Ok(())).unwrap();
        assert_eq!(
            c.unwrap().dht.restart_witness_dir,
            Some(PathBuf::from(&full))
        );
    }

    /// The refusal of an unusable data folder: its kind, the folder the
    /// Event Log names, and the temp log's reason, which names the setting
    /// and what's wrong.
    fn unusable_as(r: Checked, setting: Setting, problem: Problem, dir: &Path) -> String {
        match r {
            Err((Refusal::Unusable(s, p), why, d)) if (s, p) == (setting, problem) => {
                assert_eq!(d, dir, "{why}");
                assert!(why.contains(problem.text()), "{why}");
                assert!(why.contains(setting.source()), "{why}");
                why
            }
            other => panic!("{setting:?} {problem:?} {dir:?}: {other:?}"),
        }
    }

    fn env_data(d: &Path) -> impl Fn(&str) -> Option<String> {
        let d = d.to_string_lossy().into_owned();
        move |k: &str| (k == "LODESTAR_DATA_DIR").then(|| d.clone())
    }

    fn unasked() -> Result<(), String> {
        panic!("the ACL step ran")
    }

    /// A temp folder and its real path, with no link on the way (macOS's
    /// `/var` is one), which the checks would rightly skip.
    fn real_root() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        // Windows' temp folder has none, and icacls takes no \\?\ path.
        let real = if cfg!(windows) {
            root.path().to_path_buf()
        } else {
            root.path().canonicalize().unwrap()
        };
        (root, real)
    }

    /// X-win-data-dir-unusable-log: a data folder that is a file is refused
    /// with its own Event Log entry, whichever setting chose it, and the
    /// service's own folder before the ACL step.
    #[test]
    fn the_service_refuses_a_data_folder_that_is_a_file() {
        let (_root, root) = real_root();
        let svc = root.join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        let file = root.join("data.txt");
        std::fs::write(&file, "").unwrap();

        let why = unusable_as(
            load_checked(&svc, None, env_data(&file), unasked),
            Setting::Env,
            Problem::File,
            &file,
        );
        assert!(why.contains("set LODESTAR_DATA_DIR to a folder"), "{why}");
        // The old refusal's shape is kept for load_guarded's callers.
        assert!(matches!(
            load_guarded(&svc, None, env_data(&file), unasked),
            Err((Refusal::Unusable(Setting::Env, Problem::File), _))
        ));

        let config = root.join("service.toml");
        std::fs::write(
            &config,
            format!("data_dir = {:?}\n", file.to_string_lossy()),
        )
        .unwrap();
        let why = unusable_as(
            load_checked(&svc, Some(config), |_| None, unasked),
            Setting::File,
            Problem::File,
            &file,
        );
        assert!(why.contains("service.toml"), "{why}");

        // The service's own folder: refused before the ACL step, which
        // would otherwise fail first and point at Repair.
        std::fs::remove_dir(&svc).unwrap();
        std::fs::write(&svc, "").unwrap();
        let why = unusable_as(
            load_checked(&svc, None, |_| None, unasked),
            Setting::Default,
            Problem::File,
            &svc,
        );
        assert!(why.contains("move or rename that file"), "{why}");
    }

    /// A data folder that can't be created, because a file is in its way,
    /// is refused; one that can is created before anything uses it.
    #[test]
    fn the_service_refuses_a_data_folder_it_cant_create() {
        let (_root, root) = real_root();
        let program_data = root.join("ProgramData");
        let svc = program_data.join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        let file = root.join("data.txt");
        std::fs::write(&file, "").unwrap();
        let under_file = file.join("Lodestar");

        let why = unusable_as(
            load_checked(&svc, None, env_data(&under_file), unasked),
            Setting::Env,
            Problem::Uncreatable,
            &under_file,
        );
        assert!(why.contains("is a file"), "{why}");

        // A missing folder is created, and nests.
        let fresh = root.join("D").join("Lodestar");
        let (c, d) = load_checked(&svc, None, env_data(&fresh), unasked).unwrap();
        assert_eq!((c.unwrap().data_dir, &d), (fresh.clone(), &fresh));
        assert!(fresh.is_dir());

        // The service's own folder is never created here (the ACL step
        // creates it private), but a file in its way is refused first.
        std::fs::remove_dir_all(&program_data).unwrap();
        let created = std::cell::Cell::new(false);
        let (_, d) = load_checked(
            &svc,
            None,
            |_| None,
            || {
                created.set(true);
                std::fs::create_dir_all(&svc).map_err(|e| e.to_string())
            },
        )
        .unwrap();
        assert_eq!((d, created.get()), (svc.clone(), true));
        std::fs::remove_dir_all(&program_data).unwrap();
        std::fs::write(&program_data, "").unwrap();
        unusable_as(
            load_checked(&svc, None, |_| None, unasked),
            Setting::Default,
            Problem::Uncreatable,
            &svc,
        );
    }

    /// Makes `dir` unlistable for this process, or listable again; `false`
    /// where that didn't take (root on Unix ignores permissions). Never
    /// panics, so [`Locked`] can call it while unwinding.
    fn deny_listing(dir: &Path, deny: bool) -> bool {
        #[cfg(unix)]
        let set = {
            use std::os::unix::fs::PermissionsExt;
            let mode = if deny { 0o000 } else { 0o755 };
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).is_ok()
        };
        #[cfg(windows)]
        let set = {
            // Everyone denied listing; removed again with /remove:d.
            let how: &[&str] = if deny {
                &["/deny", "*S-1-1-0:(RD)"]
            } else {
                &["/remove:d", "*S-1-1-0"]
            };
            std::process::Command::new("icacls")
                .arg(dir)
                .args(how)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        set && std::fs::read_dir(dir).is_err() == deny
    }

    /// Makes its folder listable again when dropped, even after a failed assertion, so the
    /// temp folder can be removed.
    struct Locked<'a>(&'a Path);

    impl Drop for Locked<'_> {
        fn drop(&mut self) {
            deny_listing(self.0, false);
        }
    }

    /// A data folder the service's account can't list is refused, naming
    /// it, and the service's own folder before the ACL step.
    #[test]
    fn the_service_refuses_a_data_folder_it_cant_read() {
        let (_root, root) = real_root();
        let svc = root.join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        let _locked = Locked(&locked);
        if !deny_listing(&locked, true) {
            eprintln!("skipped: permissions don't apply to this account");
            return;
        }
        let env = unusable_as(
            load_checked(&svc, None, env_data(&locked), unasked),
            Setting::Env,
            Problem::Unreadable,
            &locked,
        );
        let _svc_locked = Locked(&svc);
        assert!(deny_listing(&svc, true));
        let default = unusable_as(
            load_checked(&svc, None, |_| None, unasked),
            Setting::Default,
            Problem::Unreadable,
            &svc,
        );
        assert!(deny_listing(&locked, false));
        assert!(deny_listing(&svc, false));
        assert!(env.contains("full control"), "{env}");
        assert!(default.contains("Repair"), "{default}");
    }

    /// Nothing is followed or created through a link: a data folder that
    /// is one, or has one on its way, is used as before.
    #[cfg(unix)]
    #[test]
    fn a_link_is_never_followed_or_created_through() {
        let (_root, root) = real_root();
        let svc = root.join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        let file = root.join("data.txt");
        std::fs::write(&file, "").unwrap();
        let to_file = root.join("to-file");
        std::os::unix::fs::symlink(&file, &to_file).unwrap();
        assert!(load_checked(&svc, None, env_data(&to_file), unasked).is_ok());
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let to_dir = root.join("to-dir");
        std::os::unix::fs::symlink(&real, &to_dir).unwrap();
        let through = to_dir.join("a").join("Lodestar");
        assert!(load_checked(&svc, None, env_data(&through), unasked).is_ok());
        assert!(!real.join("a").exists(), "created through a link");
    }

    /// The Event Log entry names the setting, the folder, what's wrong and
    /// the fix, and nothing else; with its own event ID.
    #[test]
    fn the_unusable_folder_note_names_the_setting_folder_and_fix() {
        let dir = Path::new(r"D:\Lodestar");
        for setting in [Setting::Default, Setting::Env, Setting::File] {
            for problem in [Problem::File, Problem::Unreadable, Problem::Uncreatable] {
                let refusal = Refusal::Unusable(setting, problem);
                let text = event_log_text(refusal, dir);
                for part in [r"D:\Lodestar", setting.source(), problem.text()] {
                    assert!(text.contains(part), "{part} not in {text}");
                }
                assert!(text.contains(&unusable_fix(setting, problem)), "{text}");
                assert!(
                    text.contains(r"%SystemRoot%\Temp\lodestar-service.log"),
                    "{text}"
                );
                for word in ["S-1-", "NT SERVICE", "\"", "token", "password"] {
                    assert!(!text.contains(word), "{word} in {text}");
                }
                assert!(!text.chars().any(char::is_control), "{text}");
                assert!(text.chars().count() < 1024);
                assert_eq!(refusal.event(), ("ERROR", 6));
            }
        }
        let text = |s| event_log_text(Refusal::Unusable(s, Problem::File), dir);
        assert!(text(Setting::File).contains("set data_dir to a folder"));
        assert!(text(Setting::Env).contains("set LODESTAR_DATA_DIR to a folder"));
        // The existing codes stay as they were.
        assert_eq!(Refusal::NotPrivate.event(), ("ERROR", 3));
        assert_eq!(Refusal::RollingBack.event(), ("WARNING", 4));
        assert_eq!(Refusal::NotFullPath.event(), ("ERROR", 5));
    }

    /// X-win-service-cwd-fallback: with no config.toml in its folder and
    /// nothing set, the service never reads `data\config.toml` from its
    /// working folder (the system folder under the SCM). The working folder is
    /// the process's, so the check runs in a child run of this test binary
    /// started in a folder holding such a file.
    #[test]
    fn the_service_never_reads_its_working_folders_config() {
        const CHILD: &str = "SERVICE_CWD_TEST_CHILD";
        if let Some(svc) = std::env::var_os(CHILD) {
            let svc = PathBuf::from(svc);
            let (c, d) = load_guarded(&svc, None, |_| None, || Ok(())).unwrap();
            let c = c.unwrap();
            assert_eq!(d, svc);
            assert!(
                !c.layers.in_file("http.bind"),
                "the working folder's config.toml was read"
            );
            assert!(!c.layers.in_env("LODESTAR_DATA_DIR"));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let svc = root.path().join("ProgramData").join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        let work = root.path().join("System32");
        std::fs::create_dir_all(work.join("data")).unwrap();
        std::fs::write(
            work.join("data").join("config.toml"),
            "[http]\nbind = \"127.0.0.1:1\"\n",
        )
        .unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "windows::service::tests::the_service_never_reads_its_working_folders_config",
                "--exact",
                "--nocapture",
            ])
            .current_dir(&work)
            .env(CHILD, &svc)
            .output()
            .unwrap();
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(out.status.success(), "{stdout}{stderr}");
        assert!(
            stdout.contains("test result: ok. 1 passed;"),
            "the child didn't run the test: {stdout}"
        );
    }

    #[test]
    fn the_scm_gets_codes_windows_cant_misread() {
        assert_eq!(scm_code(0), 0);
        assert_eq!(scm_code(3), 536_870_915);
        assert_eq!(scm_code(3), 0x2000_0003);
        assert_eq!(scm_code(1), 0x2000_0001);
    }

    #[test]
    fn restart_now_has_its_own_code() {
        assert_eq!(scm_code(RESTART), 0x2000_0050);
        assert_ne!(RESTART, 1);
        assert_ne!(RESTART, 3);
    }

    mod ledger {
        use super::super::restarts::*;

        const NOW: u64 = 1_762_560_000;
        const DAY: u64 = COUNTS_SECS;

        fn text(lines: &[&str]) -> Found {
            Found::Text(lines.iter().map(|l| format!("{l}\n")).collect())
        }

        #[test]
        fn a_fresh_install_or_a_clean_record_offers_it() {
            let l = Ledger::at_start(Found::Missing, false, NOW);
            assert_eq!(l.paused(), None);
            assert!(l.running);
            let clean = text(&["lodestar-service-restarts 1", "state stopped"]);
            assert_eq!(Ledger::at_start(clean, true, NOW).paused(), None);
        }

        #[test]
        fn a_recent_restart_or_unclean_stop_withdraws_it() {
            let restart = text(&[
                "lodestar-service-restarts 1",
                "state stopped",
                &format!("restart {}", NOW - 60),
            ]);
            let l = Ledger::at_start(restart, true, NOW);
            assert_eq!(l.paused(), Some(Kind::Restart));
            // The previous run never recorded its stop.
            let crashed = text(&["lodestar-service-restarts 1", "state running"]);
            let l = Ledger::at_start(crashed, true, NOW);
            assert_eq!(l.paused(), Some(Kind::UncleanStop));
            assert_eq!(l.entries, vec![(Kind::UncleanStop, NOW)]);
        }

        #[test]
        fn entries_expire_after_a_day_and_an_hour() {
            let old = text(&[
                "lodestar-service-restarts 1",
                "state stopped",
                &format!("restart {}", NOW - DAY),
                &format!("unclean_stop {}", NOW - DAY - 5),
            ]);
            let l = Ledger::at_start(old, true, NOW);
            assert_eq!(l.paused(), None);
            assert!(l.entries.is_empty());
            let almost = text(&[
                "lodestar-service-restarts 1",
                "state stopped",
                &format!("restart {}", NOW - DAY + 1),
            ]);
            assert_eq!(
                Ledger::at_start(almost, true, NOW).paused(),
                Some(Kind::Restart)
            );
            // Exactly a day old still counts: the margin keeps it an hour
            // longer than the SCM's reset period.
            assert_eq!(DAY, WINDOW_SECS + 3_600);
            let a_day = text(&[
                "lodestar-service-restarts 1",
                "state stopped",
                &format!("restart {}", NOW - WINDOW_SECS),
            ]);
            assert_eq!(
                Ledger::at_start(a_day, true, NOW).paused(),
                Some(Kind::Restart)
            );
        }

        #[test]
        fn an_entry_from_the_future_counts_from_now() {
            let future = text(&[
                "lodestar-service-restarts 1",
                "state stopped",
                &format!("restart {}", NOW + 10 * DAY),
            ]);
            let l = Ledger::at_start(future, true, NOW);
            assert_eq!(l.entries, vec![(Kind::Restart, NOW)]);
        }

        #[test]
        fn anything_off_fails_closed() {
            let bad: &[&[&str]] = &[
                &[],
                &["lodestar-service-restarts 2", "state stopped"],
                &["lodestar-service-restarts 1"],
                &["lodestar-service-restarts 1", "state paused"],
                &["lodestar-service-restarts 1", "state stopped", "restart"],
                &["lodestar-service-restarts 1", "state stopped", "restart -1"],
                &["lodestar-service-restarts 1", "state stopped", "restart +5"],
                &["lodestar-service-restarts 1", "state stopped", "restart  5"],
                &["lodestar-service-restarts 1", "state stopped", "reboot 5"],
                &[
                    "lodestar-service-restarts 1",
                    "state stopped",
                    "restart 99999999999999999999999",
                ],
            ];
            for lines in bad {
                let l = Ledger::at_start(text(lines), true, NOW);
                assert_eq!(l.paused(), Some(Kind::Unverified), "{lines:?}");
            }
            for found in [Found::Bad, Found::Missing] {
                let l = Ledger::at_start(found, true, NOW);
                assert_eq!(l.paused(), Some(Kind::Unverified));
            }
            // Too many lines is malformed too.
            let mut many = vec![
                "lodestar-service-restarts 1".to_string(),
                "state stopped".into(),
            ];
            many.extend((0..40).map(|i| format!("restart {}", NOW - i)));
            let many: Vec<&str> = many.iter().map(String::as_str).collect();
            let l = Ledger::at_start(text(&many), true, NOW);
            assert_eq!(l.paused(), Some(Kind::Unverified));
        }

        #[test]
        fn the_pause_lasts_until_the_newest_entry_leaves_the_window() {
            let dir = tempfile::tempdir().unwrap();
            let (mut l, paused) = begin(dir.path(), false, NOW);
            assert_eq!(paused, None);
            l.restarted(NOW + 10);
            end(dir.path(), &mut l, false, NOW + 20);
            let (_, paused) = begin(dir.path(), true, NOW + 70);
            assert_eq!(paused, Some(("restart", Some(NOW + 10 + DAY))));
            // That start never recorded its stop: an unclean one, newer.
            let (_, paused) = begin(dir.path(), true, NOW + 500);
            assert_eq!(paused, Some(("unclean_stop", Some(NOW + 500 + DAY))));
            // A folder that can't hold the record: for the whole run.
            let missing = dir.path().join("missing");
            let (_, paused) = begin(&missing, false, NOW);
            assert_eq!(paused, Some(("unwritable", None)));
        }

        #[test]
        fn a_run_records_its_end_and_round_trips() {
            let mut l = Ledger::at_start(Found::Missing, false, NOW);
            l.restarted(NOW + 5);
            let back = parse(&l.render()).unwrap();
            assert_eq!(back, l);
            assert!(!back.running);
            assert_eq!(back.paused(), Some(Kind::Restart));
            l.stopped();
            assert_eq!(parse(&l.render()), Some(l));
        }

        #[test]
        fn the_file_is_bounded_and_never_followed() {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(read(dir.path()), Found::Missing);
            let mut l = Ledger::at_start(Found::Missing, false, NOW);
            write(dir.path(), &l).unwrap();
            l.restarted(NOW);
            // A stale temporary file is replaced.
            std::fs::write(dir.path().join(format!("{FILE}.tmp")), "junk").unwrap();
            write(dir.path(), &l).unwrap();
            assert_eq!(read(dir.path()), Found::Text(l.render()));
            // Oversized: refused unread.
            std::fs::write(path(dir.path()), vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
            assert_eq!(read(dir.path()), Found::Bad);
            // Not UTF-8.
            std::fs::write(path(dir.path()), [0xff, 0xfe]).unwrap();
            assert_eq!(read(dir.path()), Found::Bad);
            // A folder in its place.
            std::fs::remove_file(path(dir.path())).unwrap();
            std::fs::create_dir(path(dir.path())).unwrap();
            assert_eq!(read(dir.path()), Found::Bad);
        }

        /// A link in the record's place is read as bad, and writing
        /// replaces the link instead of writing through it.
        #[cfg(unix)]
        #[test]
        fn a_link_is_refused_and_replaced() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("elsewhere.txt");
            let clean = "lodestar-service-restarts 1\nstate stopped\n";
            std::fs::write(&target, clean).unwrap();
            std::os::unix::fs::symlink(&target, path(dir.path())).unwrap();
            assert_eq!(read(dir.path()), Found::Bad);
            let tmp = dir.path().join(format!("{FILE}.tmp"));
            std::os::unix::fs::symlink(&target, &tmp).unwrap();
            let l = Ledger::at_start(read(dir.path()), true, NOW);
            write(dir.path(), &l).unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), clean);
            assert!(
                !std::fs::symlink_metadata(path(dir.path()))
                    .unwrap()
                    .is_symlink()
            );
            assert_eq!(read(dir.path()), Found::Text(l.render()));
        }
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
        assert_eq!(command("serve --fixture"), None);
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
    fn only_a_plain_worker_command_line_is_a_worker_service() {
        let worker = |config: Option<&str>| {
            Some(ServiceCommand {
                config: config.map(Into::into),
                role: Role::Worker,
            })
        };
        assert_eq!(role_of("worker"), worker(None));
        assert_eq!(role_of("--config c.toml worker"), worker(Some("c.toml")));
        // A service never joins (a token file would join on every start),
        // rotates, or uses another folder than the one the start secures.
        for other in [
            "worker --join https://c:8091",
            "worker --join https://c:8091 --token-file t",
            "worker --join https://c:8091 --token-file t --join-only",
            "worker --data D:\\w",
            "worker --rotate-key",
            "worker --help",
        ] {
            assert_eq!(role_of(other), None, "{other}");
        }
    }

    #[test]
    fn the_service_witness_dir_is_a_sibling_only_when_the_switch_is_on() {
        let off = |k: &str| (k == "ProgramData").then(|| r"D:\PD".to_string());
        let c = Config::load(None, off).unwrap();
        assert_eq!(default_witness_dir(&c, &off), None, "nothing while off");
        // `None` leaves the variable unset; `Some("")` sets it empty.
        let on = |witness: Option<&'static str>| {
            move |k: &str| match k {
                "ProgramData" => Some(r"D:\PD".to_string()),
                "LODESTAR_DHT_SAFE_RESTART" => Some("true".to_string()),
                "LODESTAR_DHT_WITNESS_DIR" => witness.map(str::to_string),
                _ => None,
            }
        };
        let sibling = Some(PathBuf::from(r"D:\PD").join("Lodestar-witness"));
        for unset in [None, Some("")] {
            let env = on(unset);
            let c = Config::load(None, env).unwrap();
            assert_eq!(default_witness_dir(&c, &env), sibling, "{unset:?}");
        }
        // A dir chosen in the environment or the file wins.
        let env = on(Some(r"E:\w"));
        let c = Config::load(None, env).unwrap();
        assert_eq!(default_witness_dir(&c, &env), None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[dht]\nrestart_witness_dir = \"F:/w\"\n").unwrap();
        let env = on(None);
        let c = Config::load(Some(&path), env).unwrap();
        assert_eq!(default_witness_dir(&c, &env), None);
    }

    /// Scripted [`acl::Ops`]: what the folder looks like, and whether
    /// creating it works.
    struct Ops {
        seen: Result<Option<acl::Inspection>, ()>,
        create: bool,
    }

    impl acl::Ops for Ops {
        fn inspect(&self, _: &Path) -> std::io::Result<Option<acl::Inspection>> {
            self.seen
                .clone()
                .map_err(|()| std::io::Error::other("denied"))
        }
        fn create(&self, _: &Path) -> std::io::Result<()> {
            if self.create {
                Ok(())
            } else {
                Err(std::io::Error::other("denied"))
            }
        }
    }

    const OWN: &str = "S-1-5-80-1-2-3-4-5";

    fn private() -> Option<acl::Inspection> {
        let item = |sddl: String| acl::Item {
            name: String::new(),
            link: false,
            sddl,
        };
        Some(acl::Inspection {
            folder: item(format!("O:SY{}", acl::private_sddl(Some(OWN)))),
            entries: Vec::new(),
            links: Vec::new(),
        })
    }

    #[test]
    fn the_service_witness_is_private_or_none_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().to_string_lossy().into_owned();
        let w = tmp.path().join("Lodestar-witness");
        let on = |k: &str| match k {
            "LODESTAR_DHT_SAFE_RESTART" => Some("true".to_string()),
            "ProgramData" => Some(pd.clone()),
            _ => None,
        };
        let config = || {
            let mut c = Config::load(None, on).unwrap();
            c.data_dir = tmp.path().join("Lodestar");
            // The native default it must never fall back to.
            c.dht.restart_witness_dir = Some(PathBuf::from("profile"));
            c
        };
        let ok = Ops {
            seen: Ok(private()),
            create: true,
        };
        let mut c = config();
        assert_eq!(apply_witness_default(&mut c, &on, Some(OWN), &ok), None);
        assert_eq!(c.dht.restart_witness_dir, Some(w.clone()));

        let foreign = || {
            let mut seen = private().unwrap();
            seen.folder.sddl = "O:S-1-5-21-9-9-9-1001D:AI(A;OICIID;FA;;;BU)".into();
            Some(seen)
        };
        for (ops, why) in [
            (
                Ops {
                    seen: Ok(foreign()),
                    create: true,
                },
                "not_private",
            ),
            (
                Ops {
                    seen: Ok(None),
                    create: false,
                },
                "create",
            ),
            (
                Ops {
                    seen: Err(()),
                    create: true,
                },
                "inspect",
            ),
        ] {
            let mut c = config();
            assert_eq!(
                apply_witness_default(&mut c, &on, Some(OWN), &ops),
                Some((w.clone(), why))
            );
            assert_eq!(c.dht.restart_witness_dir, None, "{why}");
        }

        // Never inside the data folder, which is backed up; the ACL
        // isn't even looked at.
        let unused = Ops {
            seen: Err(()),
            create: false,
        };
        for data in [tmp.path().to_path_buf(), w.clone()] {
            let mut c = config();
            c.data_dir = data;
            assert_eq!(
                apply_witness_default(&mut c, &on, Some(OWN), &unused),
                Some((w.clone(), "inside_data"))
            );
            assert_eq!(c.dht.restart_witness_dir, None);
        }

        // Off, or a dir chosen: nothing changes.
        let off = |k: &str| (k == "ProgramData").then(|| pd.clone());
        let mut c = Config::load(None, off).unwrap();
        let before = c.dht.restart_witness_dir.clone();
        assert_eq!(
            apply_witness_default(&mut c, &off, Some(OWN), &unused),
            None
        );
        assert_eq!(c.dht.restart_witness_dir, before);
    }

    /// A folder not created yet is compared through its nearest existing
    /// parent's real path, as the one that exists is: an 8.3 name
    /// (`RUNNER~1`) on Windows, which the test above meets on CI, and a
    /// link here.
    #[cfg(unix)]
    #[test]
    fn a_missing_folder_compares_with_an_existing_one() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let other = tmp.path().join("other");
        std::os::unix::fs::symlink(&data, &other).unwrap();
        let w = other.join("Lodestar-witness").join("deeper");
        assert!(within(&w, &data), "through the link");
        assert!(!within(&tmp.path().join("data-witness"), &data));
    }

    #[test]
    fn paths_compare_as_windows_spells_them() {
        let svc = Path::new(r"C:\ProgramData\Lodestar");
        for same in [
            r"c:/programdata/lodestar/",
            r"C:\ProgramData\Lodestar.",
            r"C:\ProgramData\Lodestar. .\",
            r"C:\ProgramData\Lodestar\.",
            r"C:\ProgramData\x\..\Lodestar",
            r"C:\ProgramData\\Lodestar",
            r"\\?\C:\ProgramData\Lodestar",
            r"C:\..\ProgramData\Lodestar",
        ] {
            assert!(same_dir(Path::new(same), svc), "{same}");
        }
        for other in [
            r"C:\ProgramData\Lodestar2",
            r"C:\ProgramData\Lodestar\..",
            r"C:\ProgramData\Lodestar\data",
            r"D:\ProgramData\Lodestar",
        ] {
            assert!(!same_dir(Path::new(other), svc), "{other}");
        }
        assert!(within(Path::new(r"C:\ProgramData\Lodestar.\data"), svc));
        assert!(within(Path::new(r"C:\ProgramData\x\..\Lodestar\d"), svc));
        assert!(!within(Path::new(r"C:\ProgramData\Lodestar\..\Other"), svc));
        // A verbatim path is taken literally: a folder named `Lodestar.`
        // isn't the service's.
        assert!(!same_dir(Path::new(r"\\?\C:\ProgramData\Lodestar."), svc));
        // UNC: `..` never climbs above the share.
        assert_eq!(tidy(r"\\srv\share\..\..\x", 0), r"\\srv\share\x");
    }

    /// X-win-service-path-hardening (c), on Windows: a folder is the
    /// service's by what the system says it is, however it's spelled or
    /// reached.
    #[cfg(windows)]
    #[test]
    fn a_folder_is_compared_by_what_it_is() {
        let root = tempfile::tempdir().unwrap();
        let svc = root.path().join("Lodestar");
        std::fs::create_dir_all(svc.join("data")).unwrap();
        let shown = svc.to_string_lossy().into_owned();

        // A junction to it (no privilege needed, unlike a symbolic link).
        let link = root.path().join("link");
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&svc)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(same_dir(&link, &svc));
        assert!(within(&link.join("data"), &svc));
        assert!(within(&link.join("not-yet"), &svc));

        // Another case, the verbatim form and a trailing dot.
        for spelled in [
            shown.to_uppercase(),
            format!(r"\\?\{shown}"),
            format!("{shown}."),
        ] {
            assert!(same_dir(Path::new(&spelled), &svc), "{spelled}");
        }

        // Its 8.3 short name, when the volume makes them.
        // Passed to cmd as written: Rust's quoting is for C programs.
        use std::os::windows::process::CommandExt as _;
        let out = std::process::Command::new("cmd")
            .raw_arg(r#"/C for %I in ("%LODESTAR_P%") do @echo %~sI"#)
            .env("LODESTAR_P", &svc)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let short = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !short.is_empty() && !short.eq_ignore_ascii_case(&shown) {
            assert!(same_dir(Path::new(&short), &svc), "{short}");
        }

        // A sibling with a similar name is neither.
        let sibling = root.path().join("Lodestar2");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(!same_dir(&sibling, &svc));
        assert!(!within(&sibling, &svc));
        assert_eq!(same_object(&sibling, &svc), Some(false));
        assert_eq!(same_object(&link, &svc), Some(true));
    }

    /// The system's own real path isn't tidied: an existing folder named
    /// `Lodestar.` is not `Lodestar` (`update` treats a match as the folder
    /// the service checked).
    #[test]
    fn a_real_folder_with_a_trailing_dot_is_its_own() {
        let root = tempfile::tempdir().unwrap();
        let svc = root.path().join("Lodestar");
        std::fs::create_dir_all(&svc).unwrap();
        #[cfg(windows)]
        let dotted = {
            let p = std::path::PathBuf::from(format!(r"\\?\{}.", svc.display()));
            std::fs::create_dir(&p).unwrap();
            p
        };
        #[cfg(not(windows))]
        let dotted = {
            let p = root.path().join("Lodestar.");
            std::fs::create_dir(&p).unwrap();
            p
        };
        assert!(!same_dir(&dotted, &svc));
        assert!(!within(&dotted, &svc));
        assert!(!within(&dotted.join("data"), &svc));
        #[cfg(windows)]
        assert_eq!(same_object(&dotted, &svc), Some(false));
    }

    /// Off Windows nothing is compared by file identity: the text decides.
    #[cfg(not(windows))]
    #[test]
    fn off_windows_folders_are_compared_by_text() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(same_object(root.path(), root.path()), None);
        assert!(same_dir(root.path(), root.path()));
        let sibling = root.path().with_extension("2");
        assert!(!within(&sibling, root.path()));
    }

    #[test]
    fn within_compares_whole_components() {
        assert!(within(Path::new(r"C:\A\b"), Path::new(r"c:/a")));
        assert!(within(Path::new(r"C:\A"), Path::new(r"C:\a\")));
        assert!(!within(Path::new(r"C:\Ab"), Path::new(r"C:\A")));
        assert!(!within(Path::new(r"C:\A"), Path::new(r"C:\A\b")));
    }

    /// The Event Log gets a fixed sentence and the data folder alone: no
    /// account, SID or command from the ACL check (every local user can
    /// read that log), on one line, short enough for `eventcreate`.
    #[test]
    fn the_event_log_text_names_only_the_folder_and_the_fix() {
        let dir = Path::new(r"C:\ProgramData\Lodestar");
        for refusal in [
            Refusal::NotPrivate,
            Refusal::RollingBack,
            Refusal::NotFullPath,
        ] {
            let text = event_log_text(refusal, dir);
            assert!(text.contains(r"C:\ProgramData\Lodestar"), "{text}");
            assert!(
                text.contains(r"%SystemRoot%\Temp\lodestar-service.log"),
                "{text}"
            );
            for word in [
                "S-1-",
                "icacls",
                "NT SERVICE",
                "BUILTIN",
                "*S",
                "token",
                "password",
            ] {
                assert!(!text.contains(word), "{word} in {text}");
            }
            assert!(!text.chars().any(char::is_control), "{text}");
            assert!(
                !text.contains('"'),
                "no quotes on eventcreate's line: {text}"
            );
            assert!(text.chars().count() < 1024, "{}", text.chars().count());
            let (kind, id) = refusal.event();
            assert!(matches!(kind, "ERROR" | "WARNING"));
            assert!((1..=1000).contains(&id), "eventcreate's own message range");
        }
        assert!(event_log_text(Refusal::NotPrivate, dir).contains("Repair"));
        assert_ne!(Refusal::NotPrivate.event(), Refusal::RollingBack.event());
        let path = event_log_text(Refusal::NotFullPath, dir);
        assert!(
            path.contains("full path") && path.contains("--config"),
            "{path}"
        );
        assert_eq!(Refusal::NotFullPath.event(), ("ERROR", 5));
    }

    /// A long configured folder is cut, so the message stays short.
    #[test]
    fn the_event_log_text_cuts_a_long_folder() {
        let long = format!(r"D:\{}", "é".repeat(5000));
        let text = event_log_text(Refusal::RollingBack, Path::new(&long));
        assert!(text.chars().count() < 1024, "{}", text.chars().count());
        assert!(text.contains("é…,"), "{text}");
        let short = event_log_text(Refusal::RollingBack, Path::new(r"D:\data"));
        assert!(!short.contains('…'));
    }

    /// A folder name can't forge a second line or hide text.
    #[test]
    fn the_event_log_text_flattens_the_folder_name() {
        let dir = Path::new("D:\\data\nLodestar's service started\u{202e}x\u{200b}y");
        let text = event_log_text(Refusal::RollingBack, dir);
        assert!(text.contains("data Lodestar's service started"), "{text}");
        assert!(text.contains("xy"), "{text}");
        assert!(
            !text
                .chars()
                .any(|c| c.is_control() || c == '\u{202e}' || c == '\u{200b}')
        );
    }

    /// Every format character the brief names is dropped too: the soft
    /// hyphen, the Arabic letter mark, the word joiner and invisible
    /// operators, and the deprecated format controls (X-event-log-followups).
    #[test]
    fn the_event_log_text_drops_every_format_character() {
        let hidden = ['\u{00ad}', '\u{061c}']
            .into_iter()
            .chain('\u{2060}'..='\u{2064}')
            .chain('\u{206a}'..='\u{206f}')
            .chain(['\u{200b}', '\u{202e}', '\u{2066}', '\u{feff}']);
        for c in hidden {
            let dir = format!("D:\\da{c}ta");
            let text = event_log_text(Refusal::NotPrivate, Path::new(&dir));
            assert!(!text.contains(c), "U+{:04X}: {text:?}", c as u32);
            assert!(text.contains(r"D:\data,"), "U+{:04X}: {text:?}", c as u32);
        }
        // Visible text around them stays, accents included.
        let text = event_log_text(Refusal::NotPrivate, Path::new("D:\\Données\u{2061}x"));
        assert!(text.contains(r"D:\Donnéesx,"), "{text}");
    }

    /// A hung `eventcreate` never holds up the refusal: it's killed at its
    /// bound and the call returns, well inside the test's 10 s deadline
    /// (X-svc-eventcreate-bounded).
    #[test]
    fn a_hung_event_note_never_holds_up_the_refusal() {
        let start = std::time::Instant::now();
        let got = write_event_note(
            crate::windows::process::never_exits(),
            std::time::Duration::from_millis(300),
        );
        // Killed at the bound, not ended early some other way (a spawn
        // error returns at once too, and must not pass for this).
        let e = got.unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut, "{e}");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
    }

    /// The refusal note's `eventcreate` spawns as the update helper's does:
    /// `System32`'s by absolute path, and without the relaunch token or any
    /// other `LODESTAR_*` variable this process has (rv-728 N4).
    #[cfg(windows)]
    #[test]
    fn the_event_note_spawns_as_the_helpers_does() {
        let cmd = event_note_command("ERROR", 3, "text").unwrap();
        let program = std::path::Path::new(cmd.get_program());
        let root = std::env::var_os("SystemRoot").unwrap();
        assert_eq!(
            program,
            std::path::Path::new(&root)
                .join("System32")
                .join("eventcreate.exe")
        );
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_os_string())
            .collect();
        let marker = std::ffi::OsString::from(crate::windows::relaunch::MARKER);
        assert!(removed.contains(&marker), "{removed:?}");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("LODESTAR_")
            {
                assert!(removed.contains(&k), "{k:?} reaches eventcreate");
            }
        }
        assert!(
            cmd.get_args().eq([
                "/T",
                "ERROR",
                "/ID",
                "3",
                "/L",
                "APPLICATION",
                "/SO",
                "Lodestar",
                "/D",
                "text"
            ]
            .map(std::ffi::OsStr::new))
        );
    }

    /// The update helper's rollback marker refuses the service's start.
    #[test]
    fn a_rollback_marker_refuses_the_start() {
        let data = tempfile::tempdir().unwrap();
        assert_eq!(rollback_refusal(data.path()), None);
        std::fs::create_dir(data.path().join("bin")).unwrap();
        std::fs::write(data.path().join("bin").join("rollback.json"), b"{}").unwrap();
        let why = rollback_refusal(data.path()).unwrap();
        assert!(why.contains("rollback is in progress"), "{why}");
    }

    /// The idle wait on the marker: each look reported on a channel, so
    /// the test waits for looks (with a deadline), never for time.
    fn idle_in_thread(
        data: &Path,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> (std::thread::JoinHandle<Idle>, std::sync::mpsc::Receiver<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = data.to_path_buf();
        let handle = std::thread::spawn(move || {
            idle_during_rollback(
                &dir,
                &|| {
                    let _ = tx.send(());
                    stop.load(std::sync::atomic::Ordering::SeqCst)
                },
                std::time::Duration::from_millis(5),
            )
        });
        (handle, rx)
    }

    /// Waits (10 s at most) for the idle wait to end, so a broken one fails
    /// the test instead of hanging it at the join.
    fn finishes(handle: &std::thread::JoinHandle<Idle>, rx: &std::sync::mpsc::Receiver<()>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
        }
        assert!(handle.is_finished(), "the idle wait didn't end");
    }

    fn looks(rx: &std::sync::mpsc::Receiver<()>, n: usize) {
        for _ in 0..n {
            rx.recv_timeout(std::time::Duration::from_secs(10))
                .expect("the idle wait looked again");
        }
    }

    /// With the marker, the service stays up (it keeps looking) until it's
    /// stopped, and never opens the store.
    #[test]
    fn the_marker_keeps_the_service_idle_until_it_stops() {
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir(data.path().join("bin")).unwrap();
        std::fs::write(data.path().join("bin").join("rollback.json"), b"{}").unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (handle, rx) = idle_in_thread(data.path(), stop.clone());
        looks(&rx, 3);
        assert!(!handle.is_finished());
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        finishes(&handle, &rx);
        let idle = handle.join().unwrap();
        assert_eq!(idle, Idle::Stopped);
        assert_eq!(idle.exit_code(), Some(0));
        assert!(!data.path().join("lodestar.db").exists());
    }

    /// The marker removed: the idle wait ends, and the service goes on.
    #[test]
    fn the_idle_service_ends_once_the_marker_goes() {
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir(data.path().join("bin")).unwrap();
        let marker = data.path().join("bin").join("rollback.json");
        std::fs::write(&marker, b"{}").unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (handle, rx) = idle_in_thread(data.path(), stop);
        looks(&rx, 2);
        std::fs::remove_file(&marker).unwrap();
        finishes(&handle, &rx);
        let idle = handle.join().unwrap();
        assert_eq!(idle, Idle::MarkerGone);
        // Not an exit: the caller carries on into the normal start.
        assert_eq!(idle.exit_code(), None);
        assert!(!data.path().join("lodestar.db").exists());
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
    fn compares_folders_as_windows_does() {
        let p = |s: &str| PathBuf::from(s);
        let pd = p(r"C:\ProgramData\Lodestar");
        assert!(same_dir(&pd, &p(r"c:\programdata\lodestar")));
        assert!(same_dir(&pd, &p("C:/ProgramData/Lodestar/")));
        assert!(!same_dir(&pd, &p(r"D:\Lodestar")));
        // Existing paths compare by their real path.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        std::fs::create_dir(&dir).unwrap();
        assert!(same_dir(&dir, &dir.join("..").join("data")));
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
