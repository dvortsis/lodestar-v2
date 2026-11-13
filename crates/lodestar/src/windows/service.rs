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
}

impl Refusal {
    /// `eventcreate`'s `/T` and `/ID`: within 1–1000, which its own
    /// message file renders as the text alone. 3 is the refused start's
    /// exit code (Windows service error 3).
    pub const fn event(self) -> (&'static str, u16) {
        match self {
            Refusal::NotPrivate => ("ERROR", 3),
            Refusal::RollingBack => ("WARNING", 4),
        }
    }
}

/// What the service writes to the Windows Application event log when it
/// refuses to start, or waits for a rollback. Every local user can read
/// that log, and the Legal Notice (section 5) allows only what happened,
/// how to fix it, and folder paths. So this is a fixed sentence and the
/// data folder `dir` alone: never [`super::acl`]'s own reason, which names
/// accounts and SIDs, nor anything else from the config. The full reason,
/// with the exact command, goes to `%SystemRoot%\Temp\lodestar-service.log`
/// only.
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
                Err(why) => {
                    log_refusal(&why);
                    event_log(
                        super::Refusal::NotPrivate,
                        &super::default_data_dir(std::env::var("ProgramData").ok()),
                    );
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
                        // Its message can name a path: the kind only.
                        Err(_) => {
                            let kind = match role {
                                super::Role::Serve => "serve",
                                super::Role::Worker => "worker",
                            };
                            lodestar_log::log_event!(
                                crate::logging::events::SERVICE_FAILED,
                                error = lodestar_log::Error::kind(kind)
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
    /// (paths, SIDs, account names and the fix) and the update helper's
    /// rollback marker come here; the SCM records the exit code ([`REFUSED`])
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
        for refusal in [Refusal::NotPrivate, Refusal::RollingBack] {
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
