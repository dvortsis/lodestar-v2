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
        }
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

/// For `main`: a copy started by Restart now (the portable build's
/// relaunch) that ends with `code` other than success, while a console is
/// attached, prints where the error and the log are and waits for Enter on
/// the console before the process exits. `Start Lodestar.cmd` has already
/// ended after the hand-over (exit 80), so without this the window would
/// close before anyone read why (#649's review). Anything else (a success,
/// a run not started by Restart now, the service, which has no console)
/// returns at once. `logs` is the log folder, if the config loaded.
pub fn hold_if_relaunch_failed(
    code: std::process::ExitCode,
    logs: Option<&std::path::Path>,
) -> std::process::ExitCode {
    #[cfg(windows)]
    {
        let relaunched = std::env::var_os(crate::windows::relaunch::MARKER).is_some();
        if should_hold(relaunched, code == std::process::ExitCode::SUCCESS) {
            hold_on_console(logs);
        }
    }
    #[cfg(not(windows))]
    let _ = logs;
    code
}

/// Whether [`hold_if_relaunch_failed`] waits: only a relaunched copy that
/// failed. (Whether a console is there is the console's to answer.)
#[cfg_attr(not(windows), allow(dead_code))]
fn should_hold(relaunched: bool, succeeded: bool) -> bool {
    relaunched && !succeeded
}

/// Says where to look and waits for a line on `CONIN$`. No console (the
/// service, a detached run) means `CONIN$` doesn't open: nothing waits.
#[cfg(windows)]
fn hold_on_console(logs: Option<&std::path::Path>) {
    use std::io::{BufRead, Write};
    let Ok(console) = std::fs::File::open("CONIN$") else {
        return;
    };
    let mut err = std::io::stderr();
    let _ = writeln!(err);
    let _ = writeln!(
        err,
        "Lodestar couldn't start again after Restart now. The error is above."
    );
    if let Some(logs) = logs {
        let _ = writeln!(err, "The log is in {}.", logs.display());
    }
    let _ = write!(err, "Press Enter to close this window.");
    let _ = err.flush();
    let mut line = String::new();
    let _ = std::io::BufReader::new(console).read_line(&mut line);
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

    /// #649's review: only a relaunched copy that failed waits on the
    /// console; a success, or a run Restart now didn't start, never does.
    #[test]
    fn only_a_failed_relaunch_holds_the_window() {
        assert!(should_hold(true, false));
        assert!(!should_hold(true, true));
        assert!(!should_hold(false, false));
        assert!(!should_hold(false, true));
        // Off Windows it's the identity, and never waits.
        let code = hold_if_relaunch_failed(std::process::ExitCode::FAILURE, None);
        assert_eq!(code, std::process::ExitCode::FAILURE);
    }

    /// "80" means a restart everywhere: the portable build's code is the
    /// supervisor's (#649's review).
    #[cfg(unix)]
    #[test]
    fn the_relaunch_exit_code_is_the_restart_code() {
        assert_eq!(EXIT_RELAUNCHED, crate::update::control::EXIT_RESTART);
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
