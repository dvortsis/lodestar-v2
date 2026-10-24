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
//! - **Windows: `manual`** until M6 wires its hooks (a service exits with
//!   a service-specific code for the SCM's recovery action; the portable
//!   build starts a new copy, then exits). They keep [`Restart::requested`]
//!   and the method, as B-update's supervised relaunch will.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use lodestar_api::system::{RestartInfo, RestartMethod};
use tokio::sync::Notify;

/// The process's restart state. One per process ([`Restart::global`]);
/// tests make their own.
#[derive(Debug)]
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
}

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
        }
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

    pub fn estimated_seconds(&self) -> u64 {
        match self.start_secs.load(Ordering::Relaxed) {
            0 => DEFAULT_ESTIMATE_SECS,
            n => n,
        }
    }

    /// This run can't restart itself, for `why` (shown as the hint).
    pub fn refuse(&self, why: impl Into<String>) {
        let _ = self.refused.set(why.into());
    }

    pub fn available(&self) -> bool {
        matches!(
            self.method(),
            RestartMethod::Reexec | RestartMethod::Supervised
        ) && self.refused.get().is_none()
    }

    pub fn info(&self) -> RestartInfo {
        let available = self.available();
        let hint = match self.refused.get() {
            Some(why) => Some(why.clone()),
            None => (!available).then(|| HINT.to_string()),
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

    /// Asks for a restart; [`Restart::requested`] resolves. Asking twice is
    /// asking once.
    pub fn request(&self) {
        if !self.requested.swap(true, Ordering::SeqCst) {
            // Required: a log must say why the process went away.
            lodestar_log::log_event!(crate::logging::events::APP_RESTART);
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
#[cfg(not(unix))]
const METHOD: RestartMethod = RestartMethod::Manual;

/// How to restart where the server can't (Windows, until M6 wires the
/// service's recovery action and the portable relaunch).
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

/// Windows can't restart in place yet ([`RestartMethod::Manual`]): the
/// request is refused before it gets here.
#[cfg(not(unix))]
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
