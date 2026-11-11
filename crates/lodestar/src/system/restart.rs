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
/// process it replaces to have exited ([`await_predecessor`]) before it
/// refuses to start.
#[cfg_attr(not(windows), allow(dead_code))]
pub const PREDECESSOR_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
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

/// Asks `probe` every `poll` until it says anything but
/// [`Predecessor::Running`], or `wait` has passed since the first call:
/// what it said last. `Running` then means it's still there, and the copy
/// refuses ([`predecessor_refusal`]). The clock and the sleep are passed
/// in, so the loop is tested on every platform without waiting.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn await_predecessor(
    mut probe: impl FnMut() -> Predecessor,
    wait: std::time::Duration,
    poll: std::time::Duration,
    now: impl Fn() -> Instant,
    mut sleep: impl FnMut(std::time::Duration),
) -> Predecessor {
    let start = now();
    loop {
        match probe() {
            Predecessor::Running if now().saturating_duration_since(start) < wait => sleep(poll),
            other => return other,
        }
    }
}

/// What the watch's state means: `None` with no watch; else its
/// `try_wait`, with `Some(success)` once it has ended. Only a watch that
/// ended well says the process has exited.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn watch_says(state: Option<std::io::Result<Option<bool>>>) -> Predecessor {
    match state {
        Some(Ok(None)) => Predecessor::Running,
        Some(Ok(Some(true))) => Predecessor::Exited,
        _ => Predecessor::Unknown,
    }
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

/// The Windows PowerShell the copy (`me`) runs to watch process `old`: it
/// waits on the process itself, so it ends with 0 only once `old` has
/// exited (or isn't running). A process with that id that started after
/// this copy (which `old` started) took the id later, so it isn't waited
/// on. Any error ends it non-zero ([`Predecessor::Unknown`]).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn predecessor_script(old: u32, me: u32) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'; \
         $me = (Get-Process -Id {me}).StartTime; \
         if (-not $me) {{ exit 2 }}; \
         $old = Get-Process -Id {old} -ErrorAction SilentlyContinue; \
         if ($old -and -not ($old.StartTime -gt $me)) {{ $old.WaitForExit() }}; \
         exit 0"
    )
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
    let path = crate::windows::service::service_command(std::env::args_os())?;
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

    /// X-flake-portable-restart: the wait for the process being replaced,
    /// on a fake clock that each sleep moves on. Returns the outcome and
    /// the probes made.
    fn awaited(answers: Vec<Predecessor>, wait: Duration) -> (Predecessor, usize) {
        use std::cell::Cell;
        let start = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let mut answers = answers.into_iter();
        let probes = Cell::new(0usize);
        let got = await_predecessor(
            || {
                probes.set(probes.get() + 1);
                answers.next().unwrap_or(Predecessor::Running)
            },
            wait,
            PREDECESSOR_POLL,
            || start + elapsed.get(),
            |d| elapsed.set(elapsed.get() + d),
        );
        (got, probes.get())
    }

    /// The #655 flake: the hand-over's end of file came while the old
    /// process still held the store. The copy goes on only once the
    /// process itself has exited.
    #[test]
    fn a_relaunched_copy_waits_until_the_old_process_has_exited() {
        use Predecessor::*;
        let (got, probes) = awaited(vec![Running, Running, Exited], PREDECESSOR_WAIT);
        assert_eq!((got, probes), (Exited, 3));
        // Nothing to wait on: at once.
        assert_eq!(awaited(vec![Unknown], PREDECESSOR_WAIT), (Unknown, 1));
        // A watch that stops answering mid-way isn't a reason to wait on.
        assert_eq!(
            awaited(vec![Running, Unknown], PREDECESSOR_WAIT),
            (Unknown, 2)
        );
    }

    #[test]
    fn a_process_still_running_past_the_wait_is_refused() {
        // Probed at 0, 50, …, 1,000 ms: at the deadline it's given up.
        let (got, probes) = awaited(Vec::new(), Duration::from_secs(1));
        assert_eq!((got, probes), (Predecessor::Running, 21));
        let (got, probes) = awaited(Vec::new(), Duration::ZERO);
        assert_eq!((got, probes), (Predecessor::Running, 1));
        // The brief's floor: at least 10 s before a refusal.
        assert!(PREDECESSOR_WAIT >= Duration::from_secs(10));
        let refusal = predecessor_refusal(PREDECESSOR_WAIT);
        assert!(refusal.contains("still running 30 s"), "{refusal}");
        assert!(refusal.contains("Close Lodestar and start it again."));
    }

    #[test]
    fn only_a_watch_that_ended_well_says_exited() {
        use Predecessor::*;
        assert_eq!(watch_says(Some(Ok(None))), Running);
        assert_eq!(watch_says(Some(Ok(Some(true)))), Exited);
        assert_eq!(watch_says(Some(Ok(Some(false)))), Unknown);
        assert_eq!(watch_says(Some(Err(std::io::Error::other("x")))), Unknown);
        assert_eq!(watch_says(None), Unknown);
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
        let script = predecessor_script(4242, 77);
        assert!(script.contains("Get-Process -Id 77"), "{script}");
        assert!(script.contains("Get-Process -Id 4242"), "{script}");
        assert!(script.contains("$old.WaitForExit()"), "{script}");
        assert!(script.ends_with("exit 0"), "{script}");
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
