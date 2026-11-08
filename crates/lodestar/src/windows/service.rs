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

/// Whether `inner` is `outer` or inside it, compared as [`same_dir`] does.
pub fn within(inner: &Path, outer: &Path) -> bool {
    let (i, o) = (norm(inner), norm(outer));
    i == o || i.starts_with(&format!("{o}\\"))
}

/// A real path is verbatim (`\\?\C:\…`), so the prefix goes before
/// comparing.
fn norm(p: &Path) -> String {
    let p = real(p);
    let s = p.to_string_lossy().replace('/', "\\");
    let s = match s.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => rest.to_string(),
        },
        None => s,
    };
    s.trim_end_matches('\\').to_lowercase()
}

/// `p` through its nearest existing folder's real path, with the rest
/// appended: a folder not created yet (the witness) then compares with one
/// that exists (the data folder), whatever 8.3 names or links lead there.
fn real(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = p;
    loop {
        if let Ok(mut r) = std::fs::canonicalize(at) {
            r.extend(rest.iter().rev());
            return r;
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// Whether two paths name the same folder, as Windows compares them: the
/// real paths when both exist (so case, 8.3 names and `..` don't matter),
/// else case-insensitively with `/` as `\` and no trailing separator.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    norm(a) == norm(b)
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
    // `serve --fixture` serves test data offline: never a service start.
    matches!(
        cli.command,
        None | Some(crate::cli::Command::Serve {
            fixture: None,
            child: false
        })
    )
    .then_some(cli.config)
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
        let code = match load(own.as_deref()) {
            // Refused before anything was read or written in the default
            // folder, so the reason goes outside it, beside the
            // installer's log; the exit code (3) says it was refused.
            Err(why) => {
                log_refusal(&why);
                REFUSED
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
                match config
                    .map_err(BoxError::from)
                    .and_then(|c| serve(&c, stop_rx))
                {
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
                    // Its message can name a path: the kind only.
                    Err(_) => {
                        lodestar_log::log_event!(
                            crate::logging::events::SERVICE_FAILED,
                            error = lodestar_log::Error::kind("serve")
                        );
                        1
                    }
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

    /// The config and the data folder, with `%ProgramData%\Lodestar` made
    /// private ([`acl`]) before anything in it is read, whenever it's used:
    /// for the config file when neither `--config` nor `LODESTAR_DATA_DIR`
    /// points elsewhere, and for the data when the config doesn't. An
    /// error says why it's used and isn't private. The third part is the
    /// restart witness folder when it can't be used, and why.
    #[allow(clippy::type_complexity)]
    fn load(
        own: Option<&str>,
    ) -> Result<
        (
            Result<Config, String>,
            PathBuf,
            Option<(PathBuf, &'static str)>,
        ),
        String,
    > {
        let default_dir = super::default_data_dir(std::env::var("ProgramData").ok());
        let secure = || acl::secure_dir(&default_dir, own, &acl::Windows { own_sid: own });
        let env = |k: &str| std::env::var(k).ok();
        let explicit = CONFIG_PATH.get().cloned().flatten();
        let elsewhere = explicit.is_some() || env("LODESTAR_DATA_DIR").is_some();
        if !elsewhere {
            secure()?;
        }
        let path = super::config_path(explicit, &default_dir, env);
        let mut witness_refused = None;
        let config = Config::load(path.as_deref(), env)
            .map(|mut c| {
                super::default_data_dir_unless_set(&mut c, &default_dir);
                witness_refused =
                    super::apply_witness_default(&mut c, &env, own, &acl::Windows { own_sid: own });
                c
            })
            .map_err(|e| format!("config: {e}"));
        // A config that failed to load logs to the default folder.
        let data_dir = config
            .as_ref()
            .map_or(default_dir.clone(), |c| c.data_dir.clone());
        if elsewhere && super::same_dir(&data_dir, &default_dir) {
            secure()?;
        }
        Ok((config, data_dir, witness_refused))
    }

    /// Appends a refusal's reason to `%SystemRoot%\Temp\lodestar-service.log`,
    /// where the installer logs too, never through a planted link
    /// ([`super::super::temp_log`]). Only [`acl::secure_dir`]'s refusals
    /// come here: paths, SIDs, account names and the fix.
    fn log_refusal(why: &str) {
        let _ = crate::windows::temp_log::append("service refused to start", why);
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

    fn command(s: &str) -> Option<Option<PathBuf>> {
        service_command(std::iter::once("lodestar").chain(s.split_whitespace()))
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
    fn within_compares_whole_components() {
        assert!(within(Path::new(r"C:\A\b"), Path::new(r"c:/a")));
        assert!(within(Path::new(r"C:\A"), Path::new(r"C:\a\")));
        assert!(!within(Path::new(r"C:\Ab"), Path::new(r"C:\A")));
        assert!(!within(Path::new(r"C:\A"), Path::new(r"C:\A\b")));
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
