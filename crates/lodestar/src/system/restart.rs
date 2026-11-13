//! Restarting in place (architecture §10.10.6): the request flag and its
//! future, this run's measured start time, and the re-exec.
//!
//! `POST /system/restart` (and the address timer) call [`Restart::request`].
//! `main.rs`'s `serve` runs the services until a signal or
//! [`Restart::requested`], whichever comes first, so a restart takes the
//! same graceful shutdown as a stop (the queue, the DHT state, the final
//! commit). Then, if one was requested, it calls [`reexec`].
//!
//! - **Unix (Linux, macOS, Docker, Unraid, systemd): `reexec`.** The same
//!   binary with the same arguments replaces this process image, so the
//!   process id is kept (a container's PID 1 survives) and descriptors
//!   close on exec (std and tokio open them `CLOEXEC`). Started as root,
//!   the process has already dropped to PUID/PGID, and the drop is a
//!   no-op when not started as root. If `exec` fails, `serve` fails, so
//!   the process exits non-zero and the container's or service's restart
//!   policy takes over.
//! - **Supervised (B-update, Unix):** under the supervisor, `serve
//!   --child` writes its request and exits with `EXIT_RESTART` after the
//!   graceful shutdown, and the supervisor starts the same version again,
//!   using no probation attempt. `reexec` stays for an unsupervised run
//!   (`serve --fixture DIR`).
//! - **Windows service: `service`.** `windows/service.rs` races
//!   [`Restart::requested`] with its stop channel, takes the same graceful
//!   shutdown, then exits with a dedicated service-specific code, so the
//!   SCM's recovery action starts it again after 60 s. It's offered only
//!   while Lodestar's own count of recoveries used in the last 24 h is 0
//!   ([`Restart::refuse`] otherwise), so a crash always has one left.
//! - **Windows portable: `relaunch`.** A new copy of the same program with
//!   the same arguments is started first, which waits until this process
//!   hands over and exits ([`crate::windows::relaunch`]). `POST
//!   /system/restart` starts it before answering ([`Restart::prepare`]), so
//!   a copy that can't be started is a `409 restart_failed` and this run
//!   keeps serving; any other request (the address timer) starts it in
//!   [`Restart::request`] and drops the request if it can't. After the
//!   graceful shutdown, [`reexec`] hands over and exits.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use lodestar_api::system::{RestartInfo, RestartMethod};
use tokio::sync::Notify;

/// The process's restart state. One per process ([`Restart::global`]);
/// tests make their own.
pub struct Restart {
    requested: AtomicBool,
    notify: Notify,
    method: RestartMethod,
    /// When `serve` began, and this run's measured time to listening.
    started: OnceLock<(Instant, u64)>,
    start_secs: AtomicU64,
    /// Why this run can't restart itself after all (a temporary fixture,
    /// which is deleted when the process ends).
    refused: OnceLock<String>,
    /// `serve --child` under its supervisor: the supervisor restarts it.
    supervised: AtomicBool,
    /// Run by the Windows Service Control Manager, which restarts it.
    service: AtomicBool,
    /// Not offered before this Unix time, with this hint: the service's
    /// count of recent recoveries ([`Restart::pause_until`]).
    paused: Mutex<Option<(u64, String)>>,
    /// Runs before a request takes effect (the portable build starts its
    /// successor there); an error drops the request and this run keeps
    /// serving.
    prepare: Mutex<Option<Prepare>>,
    /// The step has run for the coming request ([`Restart::prepare`]), so
    /// [`Restart::request`] doesn't run it again.
    prepared: AtomicBool,
    /// This process is a copy started by Restart now, as its environment
    /// said when this was made: `main` makes it first thing, before
    /// anything else runs, so a later change can't hide it.
    relaunched: bool,
}

impl std::fmt::Debug for Restart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Restart")
            .field("requested", &self.requested)
            .field("method", &self.method())
            .field("refused", &self.refused.get())
            .finish_non_exhaustive()
    }
}

/// [`Restart::set_prepare`]'s step.
pub type Prepare = Box<dyn FnMut() -> std::io::Result<()> + Send>;

/// How long the SCM's recovery action waits before starting the service
/// again (`packaging/windows/main.wxs`, `RestartServiceDelayInSeconds`).
pub const SERVICE_RESTART_DELAY_SECS: u64 = 60;

/// What a restart takes when nothing has been measured yet.
const DEFAULT_ESTIMATE_SECS: u64 = 5;

impl Restart {
    pub fn new(method: RestartMethod) -> Restart {
        Restart {
            requested: AtomicBool::new(false),
            notify: Notify::new(),
            method,
            started: OnceLock::new(),
            start_secs: AtomicU64::new(0),
            refused: OnceLock::new(),
            supervised: AtomicBool::new(false),
            service: AtomicBool::new(false),
            paused: Mutex::new(None),
            prepare: Mutex::new(None),
            prepared: AtomicBool::new(false),
            relaunched: relaunched_here(),
        }
    }

    /// This process is a copy started by Restart now (the portable build's
    /// relaunch), as recorded when it started.
    pub fn relaunched(&self) -> bool {
        self.relaunched
    }

    /// Not offered until Unix time `until`, with `why` as the hint
    /// meanwhile (the service, while a recovery used in the last 24 hours
    /// is still in the SCM's count). Unlike [`Restart::refuse`], it lifts
    /// by itself.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn pause_until(&self, until: u64, why: impl Into<String>) {
        *self.paused.lock().unwrap_or_else(|e| e.into_inner()) = Some((until, why.into()));
    }

    /// The pause's hint while it lasts at `now`.
    fn paused_at(&self, now: u64) -> Option<String> {
        match &*self.paused.lock().unwrap_or_else(|e| e.into_inner()) {
            Some((until, why)) if now < *until => Some(why.clone()),
            _ => None,
        }
    }

    /// This run is the Windows service (`windows/service.rs`, before
    /// `Services::start`): the SCM's recovery action restarts it.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_service(&self) {
        self.service.store(true, Ordering::SeqCst);
    }

    /// `prepare` runs once per request, before it takes effect; an error
    /// is logged and the request dropped, so this run keeps serving.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_prepare(&self, prepare: Prepare) {
        *self.prepare.lock().unwrap_or_else(|e| e.into_inner()) = Some(prepare);
    }

    /// This run is the supervisor's child (`main.rs`, before
    /// `Services::start`): a restart is the supervisor's, so it's
    /// available on any Unix and reported as `supervised`.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn set_supervised(&self) {
        self.supervised.store(true, Ordering::SeqCst);
    }

    pub fn supervised(&self) -> bool {
        self.supervised.load(Ordering::SeqCst)
    }

    fn method(&self) -> RestartMethod {
        if self.supervised() {
            RestartMethod::Supervised
        } else if self.service.load(Ordering::SeqCst) {
            RestartMethod::Service
        } else {
            self.method
        }
    }

    /// The process's own.
    pub fn global() -> &'static Arc<Restart> {
        static GLOBAL: OnceLock<Arc<Restart>> = OnceLock::new();
        GLOBAL.get_or_init(|| Arc::new(Restart::new(METHOD)))
    }

    /// Marks the start of the services (`Services::start`, whichever entry
    /// point runs them): the clock for [`Restart::listening`] and
    /// `/system`'s `started_at`. Only the first call counts.
    pub fn started(&self) {
        let _ = self
            .started
            .set((Instant::now(), crate::unix_now().max(0) as u64));
    }

    /// Marks the listener bound: the time since [`Restart::started`] is
    /// about what a restart costs.
    pub fn listening(&self) {
        if let Some((at, _)) = self.started.get() {
            let secs = at.elapsed().as_secs_f64().ceil() as u64;
            self.start_secs.store(secs.max(1), Ordering::Relaxed);
        }
    }

    /// Whether this run got as far as listening ([`Restart::listening`]):
    /// a failure after that isn't a failure at start.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn has_listened(&self) -> bool {
        self.start_secs.load(Ordering::Relaxed) > 0
    }

    /// Unix seconds `serve` began; now if it hasn't.
    pub fn started_at(&self) -> u64 {
        self.started
            .get()
            .map_or_else(|| crate::unix_now().max(0) as u64, |&(_, at)| at)
    }

    /// About how long a restart pauses crawling: this run's measured start,
    /// plus the SCM's delay for a service.
    pub fn estimated_seconds(&self) -> u64 {
        let start = match self.start_secs.load(Ordering::Relaxed) {
            0 => DEFAULT_ESTIMATE_SECS,
            n => n,
        };
        if self.method() == RestartMethod::Service {
            start.saturating_add(SERVICE_RESTART_DELAY_SECS)
        } else {
            start
        }
    }

    /// This run can't restart itself, for `why` (shown as the hint).
    pub fn refuse(&self, why: impl Into<String>) {
        let _ = self.refused.set(why.into());
    }

    pub fn available(&self) -> bool {
        self.available_at(crate::unix_now().max(0) as u64)
    }

    fn available_at(&self, now: u64) -> bool {
        matches!(
            self.method(),
            RestartMethod::Reexec
                | RestartMethod::Supervised
                | RestartMethod::Service
                | RestartMethod::Relaunch
        ) && self.refused.get().is_none()
            && self.paused_at(now).is_none()
    }

    pub fn info(&self) -> RestartInfo {
        self.info_at(crate::unix_now().max(0) as u64)
    }

    fn info_at(&self, now: u64) -> RestartInfo {
        let available = self.available_at(now);
        let hint = match (self.refused.get(), self.paused_at(now)) {
            (Some(why), _) => Some(why.clone()),
            (None, Some(why)) => Some(why),
            (None, None) => (!available).then(|| HINT.to_string()),
        };
        RestartInfo {
            available,
            estimated_seconds: self.estimated_seconds(),
            hint,
            method: if available {
                self.method()
            } else {
                RestartMethod::Manual
            },
        }
    }

    /// Readies a request before it's made, so a caller can refuse instead of
    /// promising a restart that won't come (`POST /system/restart` answers
    /// `409 restart_failed`). For [`RestartMethod::Relaunch`] it runs the
    /// [`Restart::set_prepare`] step (the portable build starts its new
    /// copy), once: later calls, and the [`Restart::request`] that follows,
    /// don't run it again. An error is logged (`app.restart_failed`) and
    /// returned, and nothing stops. Elsewhere there's nothing to ready.
    pub fn prepare(&self) -> std::io::Result<()> {
        let mut prepare = self.prepare.lock().unwrap_or_else(|e| e.into_inner());
        if self.method() != RestartMethod::Relaunch
            || self.was_requested()
            || self.prepared.load(Ordering::SeqCst)
        {
            return Ok(());
        }
        if let Some(step) = prepare.as_mut()
            && let Err(e) = step()
        {
            lodestar_log::log_event!(
                crate::logging::events::APP_RESTART_FAILED,
                error = lodestar_log::Error::io(&e)
            );
            return Err(e);
        }
        self.prepared.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Asks for a restart; [`Restart::requested`] resolves. Asking twice is
    /// asking once. For [`RestartMethod::Relaunch`], a
    /// [`Restart::set_prepare`] step runs first unless [`Restart::prepare`]
    /// already ran it, under a lock so two requests can't both run it; if
    /// it fails the request is dropped and this run keeps serving.
    pub fn request(&self) {
        {
            let mut prepare = self.prepare.lock().unwrap_or_else(|e| e.into_inner());
            if self.was_requested() {
                drop(prepare);
                self.notify.notify_waiters();
                return;
            }
            // A paused or refused service would use a recovery a crash may
            // need: whoever asks (the API checks first; an updater might
            // not), it doesn't restart.
            if self.method() == RestartMethod::Service && !self.available() {
                lodestar_log::log_event!(
                    crate::logging::events::APP_RESTART_FAILED,
                    error = lodestar_log::Error::kind("paused")
                );
                return;
            }
            if self.method() == RestartMethod::Relaunch
                && !self.prepared.load(Ordering::SeqCst)
                && let Some(step) = prepare.as_mut()
                && let Err(e) = step()
            {
                lodestar_log::log_event!(
                    crate::logging::events::APP_RESTART_FAILED,
                    error = lodestar_log::Error::io(&e)
                );
                return;
            }
            if !self.requested.swap(true, Ordering::SeqCst) {
                // Required: a log must say why the process went away.
                lodestar_log::log_event!(crate::logging::events::APP_RESTART);
            }
        }
        self.notify.notify_waiters();
    }

    pub fn was_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Resolves once a restart has been asked for (at once if it has).
    pub async fn requested(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Registered before the check, so a request in between still
            // wakes it.
            notified.as_mut().enable();
            if self.was_requested() {
                return;
            }
            notified.await;
        }
    }
}

/// Whether this process was started by Restart now: its environment holds
/// the relaunch token. Off Windows there's no relaunch.
fn relaunched_here() -> bool {
    #[cfg(windows)]
    {
        // As `wait_if_successor`: an empty token isn't one.
        std::env::var(crate::windows::relaunch::MARKER).is_ok_and(|t| !t.is_empty())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(unix)]
const METHOD: RestartMethod = RestartMethod::Reexec;
/// The portable build; the service sets [`Restart::set_service`].
#[cfg(windows)]
const METHOD: RestartMethod = RestartMethod::Relaunch;
#[cfg(not(any(unix, windows)))]
const METHOD: RestartMethod = RestartMethod::Manual;

/// How to restart where the server can't.
const HINT: &str = "Restart Lodestar the way you started it: restart the Lodestar service in \
                    Windows Services, or close the portable Lodestar and start it again.";

/// Replaces this process with the same binary and arguments. Returns only
/// if that failed.
#[cfg(unix)]
pub fn reexec() -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(exe) => program(exe, |p| p.is_file()),
        Err(e) => return e,
    };
    lodestar_log::log_event!(
        crate::logging::events::APP_REEXEC,
        path = lodestar_log::Path::new(&exe)
    );
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec()
}

/// What to exec. After the binary was replaced on disk (an in-place
/// upgrade), Linux names the running one `<path> (deleted)`: run what's
/// installed at `<path>` now, or else the running image itself.
#[cfg(unix)]
pub(crate) fn program(
    exe: std::path::PathBuf,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    let text = exe.to_string_lossy();
    match text.strip_suffix(" (deleted)") {
        Some(installed) if exists(std::path::Path::new(installed)) => installed.into(),
        Some(_) => "/proc/self/exe".into(),
        None => exe,
    }
}

/// Windows portable ([`RestartMethod::Relaunch`]): this process has shut
/// down gracefully, so the copy started at the request takes over, and
/// this process exits with [`EXIT_RELAUNCHED`], so `Start Lodestar.cmd`
/// can tell a restart from a stop and say the new copy runs on in its
/// window.
/// Returns only if no copy could take over; `serve` then fails and the
/// process exits non-zero.
#[cfg(windows)]
pub fn reexec() -> std::io::Error {
    match crate::windows::relaunch::hand_over() {
        // Like `exec`, nothing after this runs: the services have stopped
        // and logs are written as they happen.
        Ok(()) => std::process::exit(EXIT_RELAUNCHED),
        Err(e) => e,
    }
}

/// The portable build's exit code once a new copy has taken over: 80, as
/// the Unix supervisor's `EXIT_RESTART` and the service's code, so "80"
/// means a restart on every platform. `Start Lodestar.cmd` reads it.
/// Defined everywhere (that module is Unix-only, this code Windows-only)
/// so a Unix test pins the two equal.
#[cfg_attr(not(windows), allow(dead_code))]
pub const EXIT_RELAUNCHED: i32 = 80;

/// How long a copy started by Restart now waits, once handed over, for the
/// process it replaces to have exited ([`await_watch`]) before it
/// refuses to start.
#[cfg_attr(not(windows), allow(dead_code))]
pub const PREDECESSOR_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// The watch script's own bound on its `Wait-Process`. The watch starts
/// before the old process's graceful shutdown, so this bound must outlast
/// the whole hand-over ([`GO_LIMIT`](crate::windows::relaunch::GO_LIMIT),
/// then [`EXIT_LIMIT`](crate::windows::relaunch::EXIT_LIMIT)) and then
/// [`PREDECESSOR_WAIT`] three times over. Rust measures the 30 s from the
/// later of the watch's `ready` and the hand-over ([`await_watch`]), and
/// waits at most three times that, so it always ends first. A bare 30 s here would
/// run out during a slow shutdown and refuse a restart without any wait.
pub const WATCH_SCRIPT_LIMIT: std::time::Duration = std::time::Duration::from_secs(
    crate::windows::relaunch::GO_LIMIT.as_secs()
        + crate::windows::relaunch::EXIT_LIMIT.as_secs()
        + 3 * PREDECESSOR_WAIT.as_secs(),
);

/// How often it looks.
#[cfg_attr(not(windows), allow(dead_code))]
pub const PREDECESSOR_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// What the watch on the process a relaunched copy replaces says
/// (X-flake-portable-restart).
///
/// The end of file on the hand-over pipe isn't that process's exit:
/// Windows closes a dying process's handles (the pipe's write end among
/// them) before it removes its memory, so the old process's views of
/// SQLite's `-shm` file (and of the index files) are still mapped then.
/// A store opened in that gap fails: SQLite truncates the `-shm` file as
/// its first opener, and Windows refuses to truncate a mapped file
/// ("disk I/O error"). Only the process object's signal comes after all
/// of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Predecessor {
    /// It has exited, and Windows has released everything it held.
    Exited,
    /// It's still running.
    Running,
    /// Nothing to wait on: the token names no process (a hand-made one),
    /// or the watch couldn't start or failed. The copy goes ahead, as it
    /// did before the watch existed.
    Unknown,
}

/// The id of the process a relaunched copy replaces, from its hand-over
/// token ([`crate::windows::relaunch::token`]: `lodestar-<pid>-<nanos>`,
/// made by that process). `None` for any other token.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn predecessor_pid(token: &str) -> Option<u32> {
    let (pid, nanos) = token.strip_prefix("lodestar-")?.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(pid) || !digits(nanos) {
        return None;
    }
    pid.parse().ok().filter(|&pid| pid != 0)
}

/// The Windows PowerShell the copy (`me`) runs to watch process `old`
/// (X-flake-portable-restart-2 and -3). It prints one status word a line:
/// - `started` first, a bare string, so not even `Write-Output`'s module
///   has to load before it;
/// - `ready` once its cmdlets have loaded (the first `Get-Process` and
///   `Select-Object`) and the wait is about to begin: the copy measures its
///   30 s from this word ([`await_watch`]), so a cold, scanned PowerShell
///   doesn't eat into it;
/// - `exited` once `old` has exited (or isn't running), or `timeout` if
///   it's still running after [`WATCH_SCRIPT_LIMIT`], which outlasts the
///   copy's own wait.
///
/// The wait is a loop: a `Wait-Process` that comes back while `old` is
/// still there (an error, or a missed exit event) is followed by another,
/// so only the script's own clock (`New-TimeSpan` from when it was ready)
/// says `timeout`. A process with that id that started after this copy
/// (which `old` started) took the id later, so it isn't waited on. Any
/// error ends it non-zero with no status ([`await_watch`] calls that
/// skipped).
///
/// Only cmdlets, operators and a script block, no .NET method calls, so it
/// runs under Constrained Language Mode, where `$old.WaitForExit()` is
/// blocked.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn predecessor_script(old: u32, me: u32) -> String {
    let secs = WATCH_SCRIPT_LIMIT.as_secs();
    let timed_out = WATCH_EXIT_TIMED_OUT;
    format!(
        "$ErrorActionPreference = 'Stop'; \
         'started'; \
         $me = Get-Process -Id {me} | Select-Object -ExpandProperty StartTime; \
         if (-not $me) {{ exit 2 }}; \
         $t0 = Get-Date; \
         $gone = {{ \
           $p = Get-Process -Id {old} -ErrorAction SilentlyContinue; \
           if ($p) {{ ($p | Select-Object -ExpandProperty StartTime) -gt $me }} else {{ $true }} \
         }}; \
         'ready'; \
         while (-not (& $gone)) {{ \
           $left = {secs} - (New-TimeSpan -Start $t0 | Select-Object -ExpandProperty TotalSeconds); \
           if ($left -lt 1) {{ 'timeout'; exit {timed_out} }}; \
           Wait-Process -Id {old} -Timeout $left -ErrorAction SilentlyContinue; \
           if (& $gone) {{ break }}; \
           Start-Sleep -Milliseconds 250 \
         }}; \
         'exited'; \
         exit 0"
    )
}

/// The watch's command: `shell` (Windows PowerShell) running
/// [`predecessor_script`], with no input, its status lines on a pipe, its
/// errors discarded, and never the hand-over token in its environment
/// (PK-win-restart's review: the token goes to the new copy alone).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn watch_command(mut shell: std::process::Command, old: u32, me: u32) -> std::process::Command {
    use std::process::Stdio;
    shell
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg(predecessor_script(old, me))
        .env_remove(crate::windows::relaunch::MARKER)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    shell
}

/// The watch's exit code when the old process was still running at the
/// end of its wait. The copy reads it as still running even if the
/// `timeout` line was lost.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WATCH_EXIT_TIMED_OUT: i32 = 3;

/// One status word from the watch ([`predecessor_script`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum WatchSays {
    Started,
    Ready,
    Exited,
    Timeout,
}

#[cfg_attr(not(windows), allow(dead_code))]
fn watch_line(line: &str) -> Option<WatchSays> {
    // A byte-order mark, if the shell writes one, isn't part of the word.
    match line
        .trim_start_matches('\u{feff}')
        .trim_end_matches(['\r', '\n'])
    {
        "started" => Some(WatchSays::Started),
        "ready" => Some(WatchSays::Ready),
        "exited" => Some(WatchSays::Exited),
        "timeout" => Some(WatchSays::Timeout),
        _ => None,
    }
}

/// What the watch has said so far, kept by [`read_watch`]'s thread.
#[derive(Debug, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct WatchLog {
    /// When `started` arrived.
    started: Option<Instant>,
    /// When `ready` arrived, after `started`.
    ready: Option<Instant>,
    /// The last of `exited` and `timeout` after `started`, and when it
    /// arrived.
    last: Option<(WatchSays, Instant)>,
    /// The output has ended.
    done: bool,
}

/// The longest status line read, and the most lines: anything past them
/// is ignored, so a misbehaving shell can't grow memory.
const WATCH_LINE: usize = 64;
const WATCH_LINES: usize = 16;

/// Reads the watch's status lines on a thread of its own, stamping each
/// word that counts with `now()` as it arrives.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn read_watch(
    out: impl std::io::Read + Send + 'static,
    now: impl Fn() -> Instant + Send + 'static,
) -> Arc<Mutex<WatchLog>> {
    use std::io::{BufRead, Read};
    let log = Arc::new(Mutex::new(WatchLog::default()));
    let shared = log.clone();
    let spawned = std::thread::Builder::new()
        .name("restart-watch".into())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(out);
            for _ in 0..WATCH_LINES {
                let mut line = Vec::new();
                match (&mut reader)
                    .take(WATCH_LINE as u64)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let said = std::str::from_utf8(&line).ok().and_then(watch_line);
                let mut log = shared.lock().unwrap_or_else(|e| e.into_inner());
                match said {
                    Some(WatchSays::Started) if log.started.is_none() => log.started = Some(now()),
                    Some(WatchSays::Ready) if log.started.is_some() && log.ready.is_none() => {
                        log.ready = Some(now());
                    }
                    Some(word @ (WatchSays::Exited | WatchSays::Timeout))
                        if log.started.is_some() =>
                    {
                        log.last = Some((word, now()));
                    }
                    _ => {}
                }
            }
            shared.lock().unwrap_or_else(|e| e.into_inner()).done = true;
        });
    if spawned.is_err() {
        log.lock().unwrap_or_else(|e| e.into_inner()).done = true;
    }
    log
}

/// The one stderr line when the wait couldn't run (no watch, or it failed
/// or said nothing): the copy goes ahead, as before the watch existed. It
/// never carries the token.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WATCH_SKIPPED: &str = "Lodestar couldn't check that the copy it replaces had exited, so \
     it's starting without that check.";

/// The first stderr line when the old process was still running at the
/// end of the wait; [`watch_timeline`]'s line and then the refusal
/// ([`predecessor_refusal`]) follow.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WATCH_TIMED_OUT: &str = "Lodestar's wait for the copy it replaces to exit timed out.";

/// How long a watch that has exited gets for its last line to be read,
/// and a watch that said `exited` gets to end.
const WATCH_DRAIN: std::time::Duration = std::time::Duration::from_secs(2);

/// Why a wait timed out, for [`watch_timeline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimedOut {
    /// The watch said `timeout` (or exited with its code).
    Said,
    /// The copy's own clock ran out with no answer.
    NoAnswer,
}

/// The second line of a timed-out wait (X-flake-portable-restart-3): when
/// the watch said `started` and `ready`, and how the wait ended, in
/// seconds from the hand-over (`go`), so a log from a failed restart shows
/// whether PowerShell was slow to start, slow to load its cmdlets, or
/// never saw the exit. Fixed words and numbers only: never the token.
fn watch_timeline(log: &WatchLog, go: Instant, at: Instant, why: TimedOut) -> String {
    let secs = |t: Instant| {
        if t >= go {
            format!("+{:.1} s", t.duration_since(go).as_secs_f64())
        } else {
            format!("-{:.1} s", go.duration_since(t).as_secs_f64())
        }
    };
    let when = |t: Option<Instant>| t.map_or_else(|| "never".to_string(), secs);
    let end = match why {
        TimedOut::Said => "it said the copy was still running",
        TimedOut::NoAnswer => "no answer",
    };
    let end_at = match (why, log.last) {
        (TimedOut::Said, Some((WatchSays::Timeout, t))) => t,
        _ => at,
    };
    format!(
        "The check's timeline, from the hand-over: started {}, ready {}, {end} at {}.",
        when(log.started),
        when(log.ready),
        secs(end_at)
    )
}

/// The wait on the watch, once the old process has handed over
/// (X-flake-portable-restart-2 and -3): [`Predecessor::Exited`] once the
/// watch said `exited`, unless it then ended with an error.
///
/// `wait` runs from `ready`, or from the hand-over if that came later, so
/// a slow PowerShell start or cmdlet load doesn't eat into it. Until
/// `ready` arrives the copy waits at most twice `wait` from the hand-over;
/// once it has, `wait` from it, so never more than three times `wait` in
/// all.
///
/// A watch that said `exited` but hasn't ended within [`WATCH_DRAIN`] still
/// counts as exited: the word comes only after its wait saw the exit, and
/// a PowerShell slow to shut down shouldn't refuse the restart.
///
/// The note is what to write to stderr, if anything. The clock and the
/// sleep are passed in, so it's tested on every platform without waiting.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn await_watch(
    watch: Option<&mut std::process::Child>,
    log: Option<&Mutex<WatchLog>>,
    wait: std::time::Duration,
    poll: std::time::Duration,
    now: impl Fn() -> Instant,
    mut sleep: impl FnMut(std::time::Duration),
) -> (Predecessor, Option<String>) {
    let (Some(watch), Some(log)) = (watch, log) else {
        return (Predecessor::Unknown, Some(WATCH_SKIPPED.into()));
    };
    let lock = |log: &Mutex<WatchLog>| {
        let l = log.lock().unwrap_or_else(|e| e.into_inner());
        WatchLog {
            started: l.started,
            ready: l.ready,
            last: l.last,
            done: l.done,
        }
    };
    let go = now();
    let timed_out = |why: TimedOut| {
        let l = lock(log);
        let line = watch_timeline(&l, go, now(), why);
        (
            Predecessor::Running,
            Some(format!("{WATCH_TIMED_OUT}\n{line}")),
        )
    };
    let skipped = || (Predecessor::Unknown, Some(WATCH_SKIPPED.to_string()));
    loop {
        let l = lock(log);
        match l.last {
            Some((WatchSays::Timeout, _)) => return timed_out(TimedOut::Said),
            Some((WatchSays::Exited, _)) => {
                // Its end only confirms it: give it a moment, not a veto.
                let drain = now();
                loop {
                    match watch.try_wait() {
                        // Its timed-out code still wins over any line.
                        Ok(Some(status)) if status.code() == Some(WATCH_EXIT_TIMED_OUT) => {
                            return timed_out(TimedOut::Said);
                        }
                        Ok(Some(status)) if !status.success() => return skipped(),
                        Ok(Some(_)) => return (Predecessor::Exited, None),
                        Ok(None) if now().saturating_duration_since(drain) < WATCH_DRAIN => {
                            sleep(poll);
                        }
                        Ok(None) => return (Predecessor::Exited, None),
                        Err(_) => return skipped(),
                    }
                }
            }
            _ => {}
        }
        match watch.try_wait() {
            Ok(Some(status)) if status.code() == Some(WATCH_EXIT_TIMED_OUT) => {
                return timed_out(TimedOut::Said);
            }
            Ok(Some(status)) => {
                // Its last line may still be in the pipe.
                let drain = now();
                while !lock(log).done && now().saturating_duration_since(drain) < WATCH_DRAIN {
                    sleep(poll);
                }
                return match lock(log).last {
                    Some((WatchSays::Exited, _)) if status.success() => (Predecessor::Exited, None),
                    Some((WatchSays::Timeout, _)) => timed_out(TimedOut::Said),
                    _ => skipped(),
                };
            }
            Ok(None) => {}
            Err(_) => return skipped(),
        }
        let t = now();
        let out = match l.ready {
            Some(ready) => t.saturating_duration_since(ready.max(go)) >= wait,
            None => t.saturating_duration_since(go) >= wait * 2,
        };
        if out {
            return timed_out(TimedOut::NoAnswer);
        }
        sleep(poll);
    }
}

/// What a relaunched copy says when the process it replaces is still
/// running after `waited`: it ends before opening anything in the data
/// folder.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn predecessor_refusal(waited: std::time::Duration) -> String {
    format!(
        "Lodestar didn't start again after Restart now: the copy it replaces was still \
         running {} s after it shut down, so this one stopped before opening the data \
         folder. Close Lodestar and start it again.",
        waited.as_secs()
    )
}

/// Set to `1` by `Start Lodestar.cmd`, which runs in a window someone can
/// see; `lodestar.exe` and the copy Restart now starts inherit it. A run
/// started any other way (Task Scheduler, a service wrapper, a hidden
/// console) doesn't have it, so a failure there never waits (#651's
/// review).
#[cfg_attr(not(windows), allow(dead_code))]
pub const WINDOW_ENV: &str = "LODESTAR_PORTABLE_WINDOW";

/// The longest a failed relaunch waits for Enter, so it can never hang.
#[cfg_attr(not(windows), allow(dead_code))]
const HOLD_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);

/// For `main`, around everything it runs: a copy started by Restart now
/// (`relaunched`, read by `main` before anything else runs, so a config
/// that fails early still counts: [`Restart::relaunched`]) that fails at
/// start (before it was listening) in `Start Lodestar.cmd`'s window
/// ([`WINDOW_ENV`]) says where the error and the log are and waits for
/// Enter, for at most [`HOLD_LIMIT`], before the process exits. The
/// launcher has already ended after the hand-over (exit 80), so without
/// this the window would close before anyone read why (#649's review).
/// Anything else (a success, a failure after it was serving, a run not
/// started by Restart now or not in the launcher's window, no console)
/// returns at once.
pub fn hold_if_relaunch_failed(
    relaunched: bool,
    code: std::process::ExitCode,
) -> std::process::ExitCode {
    #[cfg(windows)]
    {
        let window = std::env::var(WINDOW_ENV).is_ok_and(|v| v == "1");
        if should_hold(
            relaunched,
            code == std::process::ExitCode::SUCCESS,
            Restart::global().has_listened(),
            window,
        ) && let Ok(console) = std::fs::File::open("CONIN$")
        {
            let logs = log_folder();
            prompt(logs.as_deref(), &mut std::io::stderr());
            let _ = wait_line_for(std::io::BufReader::new(console), HOLD_LIMIT);
        }
    }
    #[cfg(not(windows))]
    let _ = relaunched;
    code
}

/// The log folder: the one the log file opened in this run (a folder moved
/// in the app is honoured), or else the config's default, `<data>\logs`,
/// from the same `--config`; `None` if neither is known.
#[cfg(windows)]
fn log_folder() -> Option<std::path::PathBuf> {
    if let Some(opened) = crate::logging::runtime().and_then(|rt| rt.opened_folder()) {
        return Some(opened);
    }
    let path = crate::windows::service::service_command(std::env::args_os())?.config;
    let config = crate::config::Config::load(path.as_deref(), |k| std::env::var(k).ok()).ok()?;
    let logs = config.data_dir.join("logs");
    Some(std::path::absolute(&logs).unwrap_or(logs))
}

/// Whether [`hold_if_relaunch_failed`] waits: a relaunched copy that
/// failed at start, in the launcher's window.
#[cfg_attr(not(windows), allow(dead_code))]
fn should_hold(relaunched: bool, succeeded: bool, listened: bool, window: bool) -> bool {
    relaunched && !succeeded && !listened && window
}

/// Says where to look.
#[cfg_attr(not(windows), allow(dead_code))]
fn prompt(logs: Option<&std::path::Path>, out: &mut impl std::io::Write) {
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Lodestar couldn't start again after Restart now. The error is above."
    );
    if let Some(logs) = logs {
        let _ = writeln!(out, "The log is in {}.", logs.display());
    }
    let _ = write!(out, "Press Enter to close this window.");
    let _ = out.flush();
}

/// Reads one line from `input` (an end of file or an error counts) on a
/// thread of its own, waiting at most `limit`: whether it came in time. A
/// read still blocked at the deadline is left behind; the process is about
/// to exit.
#[cfg_attr(not(windows), allow(dead_code))]
fn wait_line_for(
    mut input: impl std::io::BufRead + Send + 'static,
    limit: std::time::Duration,
) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("relaunch-hold".into())
        .spawn(move || {
            let mut line = String::new();
            let _ = input.read_line(&mut line);
            let _ = tx.send(());
        });
    spawned.is_ok() && rx.recv_timeout(limit).is_ok()
}

/// Elsewhere it can't ([`RestartMethod::Manual`]): the request is refused
/// before it gets here.
#[cfg(not(any(unix, windows)))]
pub fn reexec() -> std::io::Error {
    std::io::Error::other("restarting in place isn't supported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn requested_fires_once_asked_and_stays_fired() {
        let r = Arc::new(Restart::new(RestartMethod::Reexec));
        let waiter = tokio::spawn({
            let r = r.clone();
            async move { r.requested().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        assert!(!r.was_requested());
        r.request();
        tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap();
        // Later waiters return at once; a second request changes nothing.
        r.request();
        tokio::time::timeout(Duration::from_secs(1), r.requested())
            .await
            .unwrap();
    }

    #[test]
    fn manual_has_a_hint_and_reexec_none() {
        let r = Restart::new(RestartMethod::Manual);
        let info = r.info();
        assert!(!info.available);
        assert!(info.hint.unwrap().contains("Windows Services"));
        let r = Restart::new(RestartMethod::Reexec);
        let info = r.info();
        assert!(info.available && info.hint.is_none());
        assert_eq!(info.estimated_seconds, DEFAULT_ESTIMATE_SECS);
    }

    #[test]
    fn a_supervised_child_restarts_through_its_supervisor() {
        let r = Restart::new(RestartMethod::Manual);
        r.set_supervised();
        let info = r.info();
        assert!(info.available && info.hint.is_none());
        assert_eq!(info.method, RestartMethod::Supervised);
    }

    #[test]
    fn a_refused_run_reports_why() {
        let r = Restart::new(RestartMethod::Reexec);
        r.refuse("A temporary fixture is deleted when Lodestar stops.");
        r.refuse("only the first reason counts");
        let info = r.info();
        assert!(!info.available && !r.available());
        assert_eq!(info.method, RestartMethod::Manual);
        assert_eq!(
            info.hint.as_deref(),
            Some("A temporary fixture is deleted when Lodestar stops.")
        );
    }

    #[test]
    fn a_service_restarts_through_the_scm_in_about_a_minute() {
        let r = Restart::new(RestartMethod::Relaunch);
        r.set_service();
        let info = r.info();
        assert!(info.available && info.hint.is_none());
        assert_eq!(info.method, RestartMethod::Service);
        assert_eq!(
            info.estimated_seconds,
            DEFAULT_ESTIMATE_SECS + SERVICE_RESTART_DELAY_SECS
        );
        // Paused by the service's count until a time: manual, with its
        // hint, then offered again by itself.
        let now = 1_762_560_000;
        r.pause_until(now + 100, "paused");
        let info = r.info_at(now);
        assert!(!info.available);
        assert_eq!(info.method, RestartMethod::Manual);
        assert_eq!(info.hint.as_deref(), Some("paused"));
        assert!(r.info_at(now + 99).hint.is_some());
        let info = r.info_at(now + 100);
        assert!(info.available && info.hint.is_none());
        assert_eq!(info.method, RestartMethod::Service);
    }

    #[test]
    fn a_paused_service_ignores_a_request() {
        let r = Restart::new(RestartMethod::Relaunch);
        r.set_service();
        r.pause_until(u64::MAX, "paused");
        r.request();
        assert!(!r.was_requested());
        let r = Restart::new(RestartMethod::Relaunch);
        r.set_service();
        r.refuse("unwritable");
        r.request();
        assert!(!r.was_requested());
    }

    #[test]
    fn a_relaunch_that_cant_start_its_copy_keeps_serving() {
        use std::sync::atomic::AtomicUsize;
        let r = Restart::new(RestartMethod::Relaunch);
        assert!(r.info().available);
        let calls = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(true));
        r.set_prepare(Box::new({
            let (calls, fail) = (calls.clone(), fail.clone());
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    Err(std::io::Error::other("spawn failed"))
                } else {
                    Ok(())
                }
            }
        }));
        r.request();
        assert!(!r.was_requested(), "no copy, no restart");
        fail.store(false, Ordering::SeqCst);
        r.request();
        assert!(r.was_requested());
        // Asked again: the copy isn't started twice.
        r.request();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Other methods never run it.
        let s = Restart::new(RestartMethod::Relaunch);
        s.set_service();
        s.set_prepare(Box::new(|| Err(std::io::Error::other("unused"))));
        s.request();
        assert!(s.was_requested());
    }

    /// X-win-restart-followups: `POST /system/restart` readies a relaunch
    /// before answering. A copy that can't be started is an error and
    /// nothing is requested; once one has started, neither a second
    /// `prepare` nor the request starts another. Other methods have
    /// nothing to ready.
    #[test]
    fn a_relaunch_starts_its_copy_before_the_answer() {
        use std::sync::atomic::AtomicUsize;
        let r = Restart::new(RestartMethod::Relaunch);
        let calls = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(true));
        r.set_prepare(Box::new({
            let (calls, fail) = (calls.clone(), fail.clone());
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    Err(std::io::Error::other("spawn failed"))
                } else {
                    Ok(())
                }
            }
        }));
        assert!(r.prepare().is_err());
        assert!(!r.was_requested(), "a failed copy requests nothing");
        fail.store(false, Ordering::SeqCst);
        assert!(r.prepare().is_ok());
        assert!(r.prepare().is_ok());
        r.request();
        assert!(r.was_requested());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one failure, one copy");
        for method in [RestartMethod::Reexec, RestartMethod::Manual] {
            let other = Restart::new(method);
            other.set_prepare(Box::new(|| Err(std::io::Error::other("unused"))));
            assert!(other.prepare().is_ok());
        }
        let service = Restart::new(RestartMethod::Relaunch);
        service.set_service();
        service.set_prepare(Box::new(|| Err(std::io::Error::other("unused"))));
        assert!(service.prepare().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn an_upgraded_binary_is_run_from_its_path() {
        use std::path::{Path, PathBuf};
        let same = program(PathBuf::from("/usr/bin/lodestar"), |_| true);
        assert_eq!(same, Path::new("/usr/bin/lodestar"));
        let installed = program(PathBuf::from("/usr/bin/lodestar (deleted)"), |p| {
            p == Path::new("/usr/bin/lodestar")
        });
        assert_eq!(installed, Path::new("/usr/bin/lodestar"));
        let gone = program(PathBuf::from("/opt/lodestar (deleted)"), |_| false);
        assert_eq!(gone, Path::new("/proc/self/exe"));
    }

    /// #649's and #651's reviews: only a relaunched copy that failed at
    /// start (before listening), in the launcher's window, waits.
    #[test]
    fn only_a_failed_relaunch_at_start_in_the_window_holds() {
        assert!(should_hold(true, false, false, true));
        for (relaunched, succeeded, listened, window) in [
            (true, true, false, true),
            (false, false, false, true),
            (true, false, true, true),
            (true, false, false, false),
        ] {
            assert!(!should_hold(relaunched, succeeded, listened, window));
        }
        let mut out = Vec::new();
        prompt(
            Some(std::path::Path::new("D:/Lodestar/data/logs")),
            &mut out,
        );
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("couldn't start again after Restart now"),
            "{text}"
        );
        assert!(
            text.contains("The log is in D:/Lodestar/data/logs."),
            "{text}"
        );
        assert!(
            text.ends_with("Press Enter to close this window."),
            "{text}"
        );
        // Off Windows the wrapper is the identity. (On Windows it would
        // wait on a developer's console; `windows_service.rs` tests it.)
        #[cfg(not(windows))]
        {
            assert!(!Restart::new(RestartMethod::Relaunch).relaunched());
            let code = hold_if_relaunch_failed(true, std::process::ExitCode::FAILURE);
            assert_eq!(code, std::process::ExitCode::FAILURE);
        }
    }

    /// The wait ends with a line or an end of file, and at the deadline
    /// when nothing comes: it can never hang (#651's review).
    #[test]
    fn the_hold_has_a_deadline() {
        use std::io::Cursor;
        let limit = Duration::from_secs(10);
        assert!(wait_line_for(Cursor::new(b"\n".to_vec()), limit));
        assert!(wait_line_for(Cursor::new(Vec::new()), limit));
        // A pipe whose writer stays open: nothing ever comes.
        let (reader, _writer) = std::io::pipe().unwrap();
        let started = std::time::Instant::now();
        assert!(!wait_line_for(
            std::io::BufReader::new(reader),
            Duration::from_millis(200)
        ));
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(started.elapsed() < limit);
    }

    #[test]
    fn listening_marks_the_start_done() {
        let r = Restart::new(RestartMethod::Relaunch);
        assert!(!r.has_listened());
        r.started();
        r.listening();
        assert!(r.has_listened());
    }

    /// "80" means a restart everywhere: the portable build's code is the
    /// supervisor's (#649's review).
    #[cfg(unix)]
    #[test]
    fn the_relaunch_exit_code_is_the_restart_code() {
        assert_eq!(EXIT_RELAUNCHED, crate::update::control::EXIT_RESTART);
    }

    /// A fake clock that each sleep moves on, from `start`.
    #[cfg(unix)]
    struct Fake {
        start: Instant,
        elapsed: std::cell::Cell<Duration>,
    }

    #[cfg(unix)]
    impl Fake {
        fn new() -> Fake {
            Fake {
                start: Instant::now(),
                elapsed: std::cell::Cell::new(Duration::ZERO),
            }
        }
        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }
        fn sleep(&self, d: Duration) {
            self.elapsed.set(self.elapsed.get() + d);
        }
    }

    #[cfg(unix)]
    fn log(started: Option<Instant>, last: Option<WatchSays>, done: bool) -> Mutex<WatchLog> {
        Mutex::new(WatchLog {
            started,
            ready: started,
            last: last.zip(started),
            done,
        })
    }

    /// `(Predecessor, note)` with the note as a `&str`, to compare.
    #[cfg(unix)]
    fn said(got: &(Predecessor, Option<String>)) -> (Predecessor, Option<&str>) {
        (got.0, got.1.as_deref())
    }

    /// The note of a timed-out wait: the fixed line, then the timeline.
    #[cfg(unix)]
    fn is_timed_out(got: &(Predecessor, Option<String>)) -> bool {
        got.0 == Predecessor::Running
            && got.1.as_deref().is_some_and(|n| {
                n.starts_with(&format!(
                    "{WATCH_TIMED_OUT}\nThe check's timeline, from the hand-over: "
                ))
            })
    }

    /// A real child that ends with `code` at once, or runs until killed.
    #[cfg(unix)]
    fn child(script: &str) -> std::process::Child {
        std::process::Command::new("sh")
            .args(["-c", script])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Waits (at most 10 s) for `c` to have exited, without reaping it.
    #[cfg(unix)]
    fn settled(c: &mut std::process::Child) -> std::process::ExitStatus {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(s) = c.try_wait().unwrap() {
                return s;
            }
            assert!(Instant::now() < end, "the child didn't exit");
            std::thread::yield_now();
        }
    }

    /// X-flake-portable-restart-2: the copy goes on only once the watch
    /// said `exited` and ended well.
    #[cfg(unix)]
    #[test]
    fn a_watch_that_says_exited_and_ends_well_is_exited() {
        let f = Fake::new();
        let mut c = child("exit 0");
        settled(&mut c);
        let l = log(Some(f.now()), Some(WatchSays::Exited), true);
        let got = await_watch(
            Some(&mut c),
            Some(&l),
            PREDECESSOR_WAIT,
            PREDECESSOR_POLL,
            || f.now(),
            |d| f.sleep(d),
        );
        assert_eq!(said(&got), (Predecessor::Exited, None));
    }

    /// The watch's last line can still be in the pipe when it has exited:
    /// the copy waits a moment for the reader before deciding.
    #[cfg(unix)]
    #[test]
    fn a_watch_that_exited_before_its_last_line_was_read_is_exited() {
        let f = Fake::new();
        let mut c = child("exit 0");
        settled(&mut c);
        let l = log(Some(f.now()), None, false);
        let got = await_watch(
            Some(&mut c),
            Some(&l),
            PREDECESSOR_WAIT,
            PREDECESSOR_POLL,
            || f.now(),
            |d| {
                f.sleep(d);
                let mut w = l.lock().unwrap();
                w.last = Some((WatchSays::Exited, f.now()));
                w.done = true;
            },
        );
        assert_eq!(said(&got), (Predecessor::Exited, None));
        // A reader that never finishes is given up on after the drain.
        let l = log(Some(f.now()), None, false);
        let start = f.now();
        let got = await_watch(
            Some(&mut c),
            Some(&l),
            PREDECESSOR_WAIT,
            PREDECESSOR_POLL,
            || f.now(),
            |d| f.sleep(d),
        );
        assert_eq!(said(&got), (Predecessor::Unknown, Some(WATCH_SKIPPED)));
        assert!(f.now().duration_since(start) >= WATCH_DRAIN);
        assert!(f.now().duration_since(start) < WATCH_DRAIN + Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn a_watch_that_fails_or_says_nothing_is_skipped_with_a_line() {
        for (code, last) in [
            ("exit 2", None),
            ("exit 0", None),
            ("exit 1", Some(WatchSays::Exited)),
        ] {
            let f = Fake::new();
            let mut c = child(code);
            settled(&mut c);
            let l = log(Some(f.now()), last, true);
            let got = await_watch(
                Some(&mut c),
                Some(&l),
                PREDECESSOR_WAIT,
                PREDECESSOR_POLL,
                || f.now(),
                |d| f.sleep(d),
            );
            assert_eq!(
                said(&got),
                (Predecessor::Unknown, Some(WATCH_SKIPPED)),
                "{code} {last:?}"
            );
        }
        // No watch at all (the token names no process, or PowerShell didn't start).
        let f = Fake::new();
        let got = await_watch(
            None,
            None,
            PREDECESSOR_WAIT,
            PREDECESSOR_POLL,
            || f.now(),
            |d| f.sleep(d),
        );
        assert_eq!(said(&got), (Predecessor::Unknown, Some(WATCH_SKIPPED)));
        assert!(!WATCH_SKIPPED.contains("lodestar-"), "never the token");
    }

    #[cfg(unix)]
    #[test]
    fn a_watch_that_timed_out_is_still_running_with_a_line() {
        let f = Fake::new();
        let mut c = child("exit 3");
        settled(&mut c);
        let l = log(Some(f.now()), Some(WatchSays::Timeout), true);
        let got = await_watch(
            Some(&mut c),
            Some(&l),
            PREDECESSOR_WAIT,
            PREDECESSOR_POLL,
            || f.now(),
            |d| f.sleep(d),
        );
        assert!(is_timed_out(&got), "{got:?}");
        assert!(
            got.1
                .as_deref()
                .unwrap()
                .ends_with("it said the copy was still running at +0.0 s."),
            "{got:?}"
        );
        // Its exit code says the same even when no line was read, or
        // when it contradicts an `exited` line.
        for last in [None, Some(WatchSays::Exited)] {
            let l = Mutex::new(WatchLog {
                started: Some(f.now()),
                last: last.map(|w| (w, f.now())),
                done: true,
                ..WatchLog::default()
            });
            let got = await_watch(
                Some(&mut c),
                Some(&l),
                PREDECESSOR_WAIT,
                PREDECESSOR_POLL,
                || f.now(),
                |d| f.sleep(d),
            );
            assert!(is_timed_out(&got), "{last:?} {got:?}");
        }
    }

    /// X-flake-portable-restart-3: the wait runs from the watch's `ready`,
    /// not from the hand-over or `started`, so a slow PowerShell start or
    /// cmdlet load doesn't eat into it. With no `ready` it ends twice the
    /// wait after the hand-over; once `ready` has come, the wait after it.
    /// `ready` arrives at its time, as the reader would stamp it.
    #[cfg(unix)]
    #[test]
    fn the_wait_runs_from_the_ready_marker() {
        // (`ready` at, seconds after the hand-over; when the wait ended)
        for (ready, waited) in [
            (None, 60),
            (Some(0), 30),
            (Some(10), 40),
            (Some(50), 80),
            (Some(70), 60),
        ] {
            let f = Fake::new();
            let go = f.now();
            let mut c = child("exec sleep 60");
            // `started` came at once; `ready` comes when the clock reaches it.
            let l = Mutex::new(WatchLog {
                started: Some(go),
                ..WatchLog::default()
            });
            let got = await_watch(
                Some(&mut c),
                Some(&l),
                PREDECESSOR_WAIT,
                PREDECESSOR_POLL,
                || f.now(),
                |d| {
                    f.sleep(d);
                    if let Some(r) = ready {
                        let at = go + Duration::from_secs(r);
                        let mut w = l.lock().unwrap();
                        if f.now() >= at && w.ready.is_none() {
                            w.ready = Some(at);
                        }
                    }
                },
            );
            let _ = c.kill();
            let _ = c.wait();
            assert!(is_timed_out(&got), "{ready:?} {got:?}");
            assert_eq!(f.elapsed.get(), Duration::from_secs(waited), "{ready:?}");
            let note = got.1.unwrap();
            let ready_text = ready
                .filter(|&r| r < 60)
                .map_or("never".to_string(), |r| format!("+{r}.0 s"));
            assert!(
                note.ends_with(&format!(
                    "started +0.0 s, ready {ready_text}, no answer at +{waited}.0 s."
                )),
                "{note}"
            );
        }
        // A `ready` before the hand-over counts from the hand-over.
        let f = Fake::new();
        let go = f.now();
        let mut c = child("exec sleep 60");
        let l = Mutex::new(WatchLog {
            started: go.checked_sub(Duration::from_secs(3)),
            ready: go.checked_sub(Duration::from_secs(1)),
            ..WatchLog::default()
        });
        let got = await_watch(
            Some(&mut c),
            Some(&l),
            PREDECESSOR_WAIT,
            PREDECESSOR_POLL,
            || f.now(),
            |d| f.sleep(d),
        );
        let _ = c.kill();
        let _ = c.wait();
        assert_eq!(f.elapsed.get(), PREDECESSOR_WAIT);
        let note = got.1.unwrap();
        assert!(
            note.ends_with("started -3.0 s, ready -1.0 s, no answer at +30.0 s."),
            "{note}"
        );
    }

    /// X-flake-portable-restart-3: `exited` is the watch's answer. A
    /// PowerShell that said it but is slow to end doesn't refuse the
    /// restart; one that ends with an error after it is a skipped check.
    #[cfg(unix)]
    #[test]
    fn a_watch_that_said_exited_is_believed_even_before_it_ends() {
        let f = Fake::new();
        let mut c = child("exec sleep 60");
        let l = log(Some(f.now()), Some(WatchSays::Exited), false);
        let got = await_watch(
            Some(&mut c),
            Some(&l),
            PREDECESSOR_WAIT,
            PREDECESSOR_POLL,
            || f.now(),
            |d| f.sleep(d),
        );
        let _ = c.kill();
        let _ = c.wait();
        assert_eq!(said(&got), (Predecessor::Exited, None));
        // It gave the watch the drain to end, and no more.
        assert!(f.elapsed.get() >= WATCH_DRAIN, "{:?}", f.elapsed.get());
        assert!(f.elapsed.get() < WATCH_DRAIN + Duration::from_secs(1));
        // Said `exited`, then failed: the check didn't finish.
        let f = Fake::new();
        let mut c = child("exit 1");
        settled(&mut c);
        let l = log(Some(f.now()), Some(WatchSays::Exited), true);
        let got = await_watch(
            Some(&mut c),
            Some(&l),
            PREDECESSOR_WAIT,
            PREDECESSOR_POLL,
            || f.now(),
            |d| f.sleep(d),
        );
        assert_eq!(said(&got), (Predecessor::Unknown, Some(WATCH_SKIPPED)));
    }

    /// The timeline line is fixed words and numbers, never the token.
    #[test]
    fn the_timeline_names_each_word_and_the_end() {
        let go = Instant::now() + Duration::from_secs(100);
        let at = |s: f64| go + Duration::from_secs_f64(s);
        let l = WatchLog {
            started: go.checked_sub(Duration::from_millis(900)),
            ready: Some(at(0.4)),
            last: Some((WatchSays::Timeout, at(1.24))),
            done: true,
        };
        assert_eq!(
            watch_timeline(&l, go, at(5.0), TimedOut::Said),
            "The check's timeline, from the hand-over: started -0.9 s, ready +0.4 s, \
             it said the copy was still running at +1.2 s."
        );
        // Its exit code with no line: the end is when the copy saw it.
        let l = WatchLog::default();
        assert_eq!(
            watch_timeline(&l, go, at(2.0), TimedOut::Said),
            "The check's timeline, from the hand-over: started never, ready never, \
             it said the copy was still running at +2.0 s."
        );
        assert_eq!(
            watch_timeline(&l, go, at(60.0), TimedOut::NoAnswer),
            "The check's timeline, from the hand-over: started never, ready never, \
             no answer at +60.0 s."
        );
    }

    #[test]
    fn the_watch_log_reads_status_after_started_only() {
        let got = |text: &'static str| {
            let l = read_watch(std::io::Cursor::new(text.as_bytes()), Instant::now);
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                let w = l.lock().unwrap();
                if w.done {
                    return (w.started.is_some(), w.last.map(|(word, _)| word));
                }
                drop(w);
                assert!(Instant::now() < end, "the reader didn't finish");
                std::thread::yield_now();
            }
        };
        assert_eq!(
            got("started\r\nexited\r\n"),
            (true, Some(WatchSays::Exited))
        );
        assert_eq!(got("started\ntimeout\n"), (true, Some(WatchSays::Timeout)));
        assert_eq!(
            got("\u{feff}started\nexited\n"),
            (true, Some(WatchSays::Exited))
        );
        assert_eq!(got("exited\n"), (false, None)); // a status before `started` doesn't count
        assert_eq!(got("started\nnoise\n"), (true, None));
        // `ready` is stamped, not a last word.
        assert_eq!(got("started\nready\n"), (true, None));
        assert_eq!(
            got("started\nready\nexited\n"),
            (true, Some(WatchSays::Exited))
        );
        assert_eq!(got(""), (false, None));
        // A long line or a flood is cut off, never read whole.
        let long: &'static str =
            Box::leak(format!("started\n{}\nexited\n", "x".repeat(10_000)).into_boxed_str());
        assert!(got(long).0);
        let flood: &'static str =
            Box::leak(format!("started\n{}exited\n", "x\n".repeat(100)).into_boxed_str());
        assert_eq!(got(flood), (true, None));
        // Only the first `started` sets the clock: a later one can't move
        // the start of the wait.
        let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let base = Instant::now();
        let l = read_watch(
            std::io::Cursor::new(&b"ready\nstarted\nstarted\nready\nready\nexited\n"[..]),
            {
                let calls = calls.clone();
                move || {
                    let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    base + Duration::from_secs(10 * (n + 1))
                }
            },
        );
        let end = Instant::now() + Duration::from_secs(10);
        while !l.lock().unwrap().done {
            assert!(Instant::now() < end, "the reader didn't finish");
            std::thread::yield_now();
        }
        let w = l.lock().unwrap();
        // A `ready` before `started` doesn't count; the first after it does.
        assert_eq!(w.started, Some(base + Duration::from_secs(10)));
        assert_eq!(w.ready, Some(base + Duration::from_secs(20)));
        assert_eq!(
            w.last,
            Some((WatchSays::Exited, base + Duration::from_secs(30)))
        );
    }

    /// Under Constrained Language Mode `$old.WaitForExit()` is blocked, so
    /// the script uses cmdlets, operators and a script block only. It
    /// prints `started` before anything that can be slow, and `ready` once
    /// its cmdlets have loaded, before the wait.
    #[test]
    fn the_script_is_safe_under_constrained_language_mode() {
        let script = predecessor_script(4242, 77);
        let secs = WATCH_SCRIPT_LIMIT.as_secs();
        // The script's bound outlasts the whole hand-over and the copy's
        // own wait after it (up to three times the wait), so only Rust's
        // clock ends it.
        assert!(
            WATCH_SCRIPT_LIMIT
                >= crate::windows::relaunch::GO_LIMIT
                    + crate::windows::relaunch::EXIT_LIMIT
                    + PREDECESSOR_WAIT * 3,
            "{WATCH_SCRIPT_LIMIT:?}"
        );
        // Wait-Process takes at most 32767 s.
        assert!(secs <= 32_767, "{WATCH_SCRIPT_LIMIT:?}");
        // X-flake-portable-restart-3: the wait is a loop on the script's
        // own clock, so a `Wait-Process` that returns early is waited
        // again, and only the clock says `timeout`.
        assert!(
            script.contains(&format!(
                "$left = {secs} - (New-TimeSpan -Start $t0 | Select-Object -ExpandProperty TotalSeconds); "
            )),
            "{script}"
        );
        assert!(
            script.contains("if ($left -lt 1) { 'timeout'; exit 3 }; "),
            "{script}"
        );
        assert!(script.contains("while (-not (& $gone)) { "), "{script}");
        assert!(
            script.contains("Wait-Process -Id 4242 -Timeout $left -ErrorAction SilentlyContinue; "),
            "{script}"
        );
        assert!(script.contains("if (& $gone) { break }; "), "{script}");
        // It never spins: a pause before each further `Wait-Process`.
        assert!(script.contains("Start-Sleep -Milliseconds 250"), "{script}");
        // A process with the old id but started after this copy is a
        // reused id, not the old process, before and after each wait.
        assert_eq!(script.matches("-gt $me").count(), 1, "{script}");
        assert_eq!(
            script.matches("Get-Process -Id 4242 ").count(),
            1,
            "{script}"
        );
        assert!(!script.contains(".WaitForExit("), "{script}");
        assert!(!script.contains("::"), "no .NET static calls: {script}");
        assert!(
            !script.contains(").") && !script.contains("$old.") && !script.contains("$p."),
            "no .NET member access: {script}"
        );
        // The order of the words: `started` before any cmdlet, `ready`
        // after the cmdlets have loaded and before the wait.
        let at = |needle: &str| {
            script
                .find(needle)
                .unwrap_or_else(|| panic!("{needle}: {script}"))
        };
        assert!(!script.contains("Write-Output"), "bare words: {script}");
        assert!(at("'started'; ") < at("Get-Process"), "{script}");
        assert!(at("Select-Object") < at("'ready'; "), "{script}");
        assert!(at("New-TimeSpan") > at("'ready'; "), "{script}");
        assert!(at("'ready'; ") < at("while ("), "{script}");
        assert!(script.trim_end().ends_with("'exited'; exit 0"), "{script}");
    }

    /// The token goes to the new copy alone, never to the watch.
    #[test]
    fn the_watch_gets_no_token() {
        let cmd = watch_command(std::process::Command::new("powershell.exe"), 4242, 77);
        let removed = cmd
            .get_envs()
            .any(|(k, v)| k == crate::windows::relaunch::MARKER && v.is_none());
        assert!(removed, "the token is removed from the watch's environment");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &args[..4],
            ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
        );
        assert_eq!(args[4], predecessor_script(4242, 77));
    }

    /// The real script on Windows, under Constrained Language Mode: it
    /// prints `started` and `ready`, waits on the old process with
    /// `Wait-Process`, and says `exited` once it has gone. The mode is checked to be in force.
    #[cfg(windows)]
    #[test]
    fn the_script_runs_under_constrained_language_mode() {
        use std::process::{Command, Stdio};
        let ping = |n: &str| {
            Command::new("ping")
                .args(["-n", n, "127.0.0.1"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        };
        // `old` runs until it's killed, so the script must be waiting on it
        // when it has said `started`.
        // Long enough to outlast a cold start: both are killed anyway.
        let mut old = ping("600");
        let mut me = ping("600"); // started after `old`
        let clm = "$ExecutionContext.SessionState.LanguageMode = 'ConstrainedLanguage'; ";
        let shell = |script: String| {
            let mut cmd = Command::new("powershell.exe");
            cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
                .arg(format!("{clm}Invoke-Expression @'\n{script}\n'@"))
                .stdin(Stdio::null())
                .stderr(Stdio::null());
            cmd
        };
        // The mode really is in force: a .NET static call fails under it.
        let probe = shell(
            "$ErrorActionPreference = 'Stop'; \
             [System.Diagnostics.Process]::GetCurrentProcess() | Out-Null; exit 0"
                .into(),
        )
        .stdout(Stdio::null())
        .status()
        .unwrap();
        assert!(!probe.success(), "Constrained Language Mode isn't in force");
        let mut watch = shell(predecessor_script(old.id(), me.id()))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        let out = watch.stdout.take().unwrap();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if tx.send(watch_line(&line)).is_err() {
                    break;
                }
            }
        });
        let first = lines.recv_timeout(Duration::from_secs(60));
        let ready = lines.recv_timeout(Duration::from_secs(60));
        let waiting = watch.try_wait().unwrap().is_none();
        let _ = old.kill();
        let _ = old.wait();
        let next = lines.recv_timeout(Duration::from_secs(30));
        let end = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(s) = watch.try_wait().unwrap() {
                break Some(s);
            }
            if Instant::now() >= end {
                break None;
            }
            std::thread::yield_now();
        };
        if status.is_none() {
            let _ = watch.kill();
            let _ = watch.wait();
        }
        let _ = me.kill();
        let _ = me.wait();
        // `started` is streamed before the wait ends, and the script is
        // still waiting on `old` then.
        assert_eq!(first, Ok(Some(WatchSays::Started)));
        assert_eq!(ready, Ok(Some(WatchSays::Ready)));
        assert!(waiting, "the script ended before `old` did");
        assert_eq!(next, Ok(Some(WatchSays::Exited)));
        let status = status.expect("the script didn't end once `old` had");
        assert!(status.success(), "{status:?}");
    }

    #[test]
    fn the_refusal_names_the_wait() {
        // The brief's floor: at least 10 s before a refusal.
        assert!(PREDECESSOR_WAIT >= Duration::from_secs(10));
        let refusal = predecessor_refusal(PREDECESSOR_WAIT);
        assert!(refusal.contains("still running 30 s"), "{refusal}");
        assert!(refusal.contains("Close Lodestar and start it again."));
    }

    /// The old process's id comes from its own token, so the hand-over
    /// needs nothing new.
    #[test]
    fn the_token_names_the_process_it_came_from() {
        let token = crate::windows::relaunch::token();
        assert_eq!(predecessor_pid(&token), Some(std::process::id()));
        assert_eq!(predecessor_pid("lodestar-4242-17"), Some(4242));
        for other in [
            "",
            "lodestar-test-token",
            "lodestar-4242",
            "lodestar--17",
            "lodestar-4242-",
            "lodestar-+42-17",
            "lodestar-0-17",
            "lodestar-99999999999-17",
            "other-4242-17",
            "lodestar-42-17-x",
        ] {
            assert_eq!(predecessor_pid(other), None, "{other:?}");
        }
    }

    #[test]
    fn the_estimate_is_this_runs_start_time() {
        let r = Restart::new(RestartMethod::Reexec);
        r.listening(); // before `started`: nothing measured
        assert_eq!(r.estimated_seconds(), DEFAULT_ESTIMATE_SECS);
        r.started();
        r.listening();
        // Under a second rounds up to one.
        assert_eq!(r.estimated_seconds(), 1);
        assert!(r.started_at() > 1_700_000_000);
    }
}
