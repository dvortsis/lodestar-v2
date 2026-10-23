//! `lodestar serve` on Unix (§12.2): a minimal init and the supervisor of
//! the child that does the work.
//!
//! It never opens the store for writing (a rollback reads its schema,
//! read-only, and renames a snapshot over it), never reads `trust.json`,
//! never starts the log file (console only, prefixed `supervisor:`) and
//! never touches the network. It forwards `TERM`, `INT`, `HUP`, `QUIT`, `USR1` and `USR2` to
//! the child and reaps every child (orphans included) on `SIGCHLD`.
//! Children are spawned with `std::process::Command` and never waited on
//! through `Child::wait`, which the reaper would race.
//!
//! At every start it first finishes a rollback journal, honours
//! `halted.json`, then chooses what to run: the active overlay when it's
//! newer than this build and verifies, else its own code. Outside
//! probation, when the child exits, it exits with the child's status
//! (128 + n for a signal), so Docker's or systemd's restart policy decides.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use lodestar_update::semver::Version;
use rustix::process::{Pid, Signal, WaitOptions};
use tokio::signal::unix::{SignalKind, signal};

use super::control::{self, Action, Hint};
use super::journal::{self, Ctx, NotBegun, Unfinished};
use super::layout::{self, Bin};
use super::overlay::{self, Refusal};
use super::pending::{
    self, ATTEMPTS, Active, Clock, Halted, Kind, Limits, Pending, Probation, Verdict,
};
use super::{build_version, testing};

/// "another Lodestar is using this data folder".
const EXIT_REFUSED: u8 = 3;

macro_rules! say {
    ($($t:tt)*) => { eprintln!("supervisor: {}", format_args!($($t)*)) };
}

/// Runs the supervisor for the data folder `data_dir`; `config` is the
/// `--config` it was given, passed on to the child.
pub fn run(
    data_dir: &Path,
    db: PathBuf,
    index_root: PathBuf,
    config: Option<&Path>,
    dropped: &crate::privdrop::Report,
) -> ExitCode {
    // Console only, never the log file: what the PUID/PGID drop did is
    // this process's to say (its child starts unprivileged).
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .try_init();
    dropped.log(data_dir);
    let bin = match Bin::open(data_dir) {
        Ok(b) => b,
        Err(e) => {
            say!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let _lock = match bin.lock() {
        Ok(Some(l)) => l,
        Ok(None) => {
            say!(
                "another Lodestar is using this data folder ({})",
                data_dir.display()
            );
            return ExitCode::from(EXIT_REFUSED);
        }
        Err(e) => {
            say!("locking {}: {e}", bin.path(".lock").display());
            return ExitCode::FAILURE;
        }
    };
    // Orphans are ours to reap even when we aren't PID 1 (systemd, a
    // shell): Linux reparents them to the nearest subreaper.
    #[cfg(target_os = "linux")]
    let _ = rustix::process::set_child_subreaper(Some(rustix::process::getpid()));
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            say!("starting: {e}");
            return ExitCode::FAILURE;
        }
    };
    let own = match own_program() {
        Ok(p) => p,
        Err(e) => {
            say!("finding this program: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut s = Supervisor {
        bin,
        db,
        index_root,
        args: child_args(config),
        own,
        image: build_version(),
        limits: testing::limits(),
        stopping: false,
    };
    runtime.block_on(s.supervise())
}

/// The child's arguments, built from what was parsed rather than copied:
/// `serve --child`, and the `--config` the supervisor was given (the
/// environment is inherited).
fn child_args(config: Option<&Path>) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(c) = config {
        args.extend(["--config".into(), c.as_os_str().to_owned()]);
    }
    args.extend(["serve".into(), "--child".into()]);
    args
}

/// This build's own code. On Linux `/proc/self/exe`: the very file this
/// supervisor runs from, even after a package upgrade replaced it on disk,
/// so the child is always the version the supervisor reports as its own.
fn own_program() -> std::io::Result<PathBuf> {
    if cfg!(target_os = "linux") {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    std::env::current_exe().map(|exe| crate::system::restart::program(exe, |p| p.is_file()))
}

struct Supervisor {
    bin: Bin,
    db: PathBuf,
    index_root: PathBuf,
    args: Vec<OsString>,
    own: PathBuf,
    image: Version,
    limits: Limits,
    /// A stop signal arrived: exit once the child has.
    stopping: bool,
}

/// What one child run ended with.
struct Ended {
    status: i32,
    /// It exited after a stop the supervisor forwarded.
    forwarded: bool,
    /// Probation stopped it (deadline, stall).
    failed: Option<&'static str>,
    passed: bool,
}

/// What to run next.
struct Plan {
    program: PathBuf,
    version: Version,
    /// Holds a verified overlay's binary open until it has started.
    _file: Option<std::fs::File>,
    notices: Vec<String>,
}

struct Signals {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
    usr1: tokio::signal::unix::Signal,
    usr2: tokio::signal::unix::Signal,
    chld: tokio::signal::unix::Signal,
}

impl Signals {
    fn new() -> std::io::Result<Signals> {
        Ok(Signals {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
            quit: signal(SignalKind::quit())?,
            hup: signal(SignalKind::hangup())?,
            usr1: signal(SignalKind::user_defined1())?,
            usr2: signal(SignalKind::user_defined2())?,
            chld: signal(SignalKind::child())?,
        })
    }
}

impl Supervisor {
    async fn supervise(&mut self) -> ExitCode {
        let mut signals = match Signals::new() {
            Ok(s) => s,
            Err(e) => {
                say!("installing signal handlers: {e}");
                return ExitCode::FAILURE;
            }
        };
        let mut notices: Vec<String> = Vec::new();
        loop {
            if let Some(code) = self.finish_journal(&mut signals).await {
                return code;
            }
            if let Some(code) = self.honour_halt() {
                return code;
            }
            if let Some(code) = self.repair_pending() {
                return code;
            }
            let plan = self.plan(std::mem::take(&mut notices));
            // Out of attempts but not yet rolled back (a hard kill landed
            // in between): roll back now rather than start it a 4th time.
            if let Some(next) = self.out_of_attempts(&plan.version) {
                match self.go(next, &mut signals, &mut notices).await {
                    Some(code) => return code,
                    None => continue,
                }
            }
            let pending = self.charge(&plan.version);
            let run_id = control::new_run_id();
            let ended = match self.spawn(&plan, &run_id) {
                Ok(pid) => {
                    drop(plan._file);
                    self.watch(pid, &run_id, pending.as_ref(), &mut signals)
                        .await
                }
                // A start that fails is a failed attempt, like a crash.
                Err(e) => {
                    say!("starting {}: {e}", plan.program.display());
                    if pending.is_none() {
                        return ExitCode::FAILURE;
                    }
                    Ended {
                        status: 127,
                        forwarded: false,
                        failed: Some("it couldn't be started"),
                        passed: false,
                    }
                }
            };
            // It passed if it cleared `pending`, whether or not a tick saw it.
            let mut ended = ended;
            if pending.is_some() && !self.bin.exists("pending") {
                ended.passed = true;
                self.passed();
            }
            if self.stopping && !ended.forwarded {
                say!("stopping as asked");
                return exit_code(ended.status);
            }
            let request = control::take(&self.bin, &run_id, Some(ended.status));
            let next = self.after(&plan.version, pending, ended, request, &mut notices);
            if let Some(code) = self.go(next, &mut signals, &mut notices).await {
                return code;
            }
        }
    }

    /// Carries out `next`; `Some` to exit.
    async fn go(
        &mut self,
        next: Next,
        signals: &mut Signals,
        notices: &mut Vec<String>,
    ) -> Option<ExitCode> {
        match next {
            Next::Again => None,
            Next::Exit(code) => Some(code),
            Next::Rollback(r) => self.begin_rollback(r, signals, notices).await,
        }
    }

    /// Begins a rollback (step 2, which verifies the snapshot: minutes on
    /// a large store) on a blocking thread while stop signals are watched;
    /// a stop then leaves the journal for the next start. A refused
    /// rollback halts after an automatic failure, and keeps the running
    /// version after a requested one.
    async fn begin_rollback(
        &mut self,
        r: Rollback,
        signals: &mut Signals,
        notices: &mut Vec<String>,
    ) -> Option<ExitCode> {
        let ctx = self.ctx();
        let now = crate::unix_now().max(0) as u64;
        let job = r.clone();
        let mut work = tokio::task::spawn_blocking(move || {
            journal::begin(
                &ctx,
                &job.failed,
                &job.previous,
                job.reason,
                job.index_before.as_deref(),
                job.lost,
                now,
            )
        });
        let done = loop {
            tokio::select! {
                r = &mut work => break r,
                _ = signals.term.recv() => {}
                _ = signals.int.recv() => {}
                _ = signals.quit.recv() => {}
            }
            self.stopping = true;
        };
        match done {
            Ok(Ok(_)) => None,
            Ok(Err(NotBegun::Refused(why))) if r.reason == "failed" => {
                say!("{} can't be rolled back: {why}", r.failed);
                let halted = Halted {
                    failed: r.failed,
                    previous: r.previous,
                    at: now,
                    refused: Some(why),
                };
                if let Err(e) = self.bin.write("halted.json", &halted) {
                    say!("writing halted.json: {e}");
                }
                let _ = self.bin.remove("pending");
                self.honour_halt();
                Some(ExitCode::FAILURE)
            }
            Ok(Err(NotBegun::Refused(why))) => {
                say!("not rolling {} back: {why}", r.failed);
                notices.push("rollback_refused".into());
                if self.stopping {
                    return Some(ExitCode::FAILURE);
                }
                None
            }
            Ok(Err(e)) => {
                say!("{e}");
                Some(ExitCode::FAILURE)
            }
            Err(e) => {
                say!("beginning the rollback: {e}");
                Some(ExitCode::FAILURE)
            }
        }
    }

    /// Finishes a journal left by an earlier run (or just begun), on a
    /// blocking thread while stop signals are watched: one finishes the
    /// current step, then stops. `Some` to exit.
    async fn finish_journal(&mut self, signals: &mut Signals) -> Option<ExitCode> {
        let j = match journal::current(&self.bin) {
            Ok(Some(j)) => j,
            Ok(None) => return None,
            Err(e) => {
                say!("reading the rollback journal: {e}. Nothing will start until it can be read.");
                return Some(ExitCode::FAILURE);
            }
        };
        say!("finishing the rollback of {} to {}", j.failed, j.previous);
        let ctx = self.ctx();
        let stop = ctx.stop.clone();
        let mut work = tokio::task::spawn_blocking(move || journal::finish(&ctx, j));
        let done = loop {
            tokio::select! {
                r = &mut work => break r,
                _ = signals.term.recv() => {}
                _ = signals.int.recv() => {}
                _ = signals.quit.recv() => {}
            }
            self.stopping = true;
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        };
        let done = done.unwrap_or_else(|e| Err(Unfinished::Failed(e.to_string())));
        if self.stopping && done.is_ok() {
            say!("stopped after the rollback finished; the previous version starts next time");
            return Some(ExitCode::FAILURE);
        }
        match done {
            Ok(()) => None,
            Err(Unfinished::Stopped) => {
                say!("stopped during a rollback; it finishes at the next start");
                Some(ExitCode::FAILURE)
            }
            Err(Unfinished::Failed(why)) => {
                say!("the rollback failed: {why}. Neither version will start until it's fixed.");
                Some(ExitCode::FAILURE)
            }
        }
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            bin: self.bin.clone(),
            db: self.db.clone(),
            index_root: self.index_root.clone(),
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(self.stopping)),
            image_version: self.image.to_string(),
        }
    }

    /// A damaged `pending` (unreadable, or a format this build doesn't
    /// know) starts `active`'s version on a fresh probation (attempts 0),
    /// so its migration is recorded and its self-test runs; with no
    /// `active` it goes. `Some` to exit: it couldn't be rewritten.
    fn repair_pending(&self) -> Option<ExitCode> {
        let Err(e) = pending::read(&self.bin) else {
            return None;
        };
        let active: Option<Active> = self.bin.read("active").ok().flatten();
        let Some(a) = active.filter(|a| layout::parse_version(&a.version).is_some()) else {
            say!("{e}: removing it");
            return match self.bin.remove("pending") {
                Ok(()) => None,
                Err(e) => {
                    say!("removing pending: {e}");
                    Some(ExitCode::FAILURE)
                }
            };
        };
        say!("{e}: {} starts a fresh probation", a.version);
        let fresh = Pending {
            format: pending::PENDING_FORMAT,
            version: a.version,
            previous: a.previous,
            kind: Kind::Install,
            attempts: 0,
            used_ms: 0,
            index_before: journal::list_index(&self.index_root),
        };
        match self.bin.write("pending", &fresh) {
            Ok(()) => None,
            Err(e) => {
                say!("rewriting pending: {e}. Nothing will start until it can be written.");
                Some(ExitCode::FAILURE)
            }
        }
    }

    /// `halted.json`: stop, unless this image is newer than both versions.
    /// An unreadable one stops too.
    fn honour_halt(&self) -> Option<ExitCode> {
        let halted: Halted = match self.bin.read("halted.json") {
            Ok(h) => h?,
            Err(e) => {
                say!("reading halted.json: {e}. Nothing will start until it can be read.");
                return Some(ExitCode::FAILURE);
            }
        };
        let newer = |v: &str| layout::parse_version(v).is_none_or(|v| self.image > v);
        if newer(&halted.failed) && newer(&halted.previous) {
            say!(
                "a newer image ({}) replaces the halted versions",
                self.image
            );
            let _ = self.bin.remove("halted.json");
            let _ = self.bin.remove("active");
            let _ = self.bin.remove("pending");
            return None;
        }
        match &halted.refused {
            Some(why) => say!(
                "{} failed {ATTEMPTS} times and can't be rolled back to {} ({why}). Lodestar \
                 won't start either again: see \"If an update fails\" in docs/operations.md, \
                 or install a newer image.",
                halted.failed,
                halted.previous
            ),
            None => say!(
                "{} failed, and so did {} after rolling back to it. Lodestar won't start either \
                 again: see \"If an update fails\" in docs/operations.md, or install a newer \
                 image.",
                halted.failed,
                halted.previous
            ),
        }
        say!(
            "Once the cause is fixed, delete {} to try again.",
            self.bin.path("halted.json").display()
        );
        Some(ExitCode::FAILURE)
    }

    /// What to run: the active overlay when it's newer than this build
    /// (and than the version it replaced) and verifies, else this build's
    /// own code. Versions only go up.
    fn plan(&self, mut notices: Vec<String>) -> Plan {
        pending::prune_skipped(&self.bin, &self.image);
        let own = |notices| Plan {
            program: self.own.clone(),
            version: self.image.clone(),
            _file: None,
            notices,
        };
        let active: Option<Active> = self.bin.read("active").ok().flatten();
        let target = active
            .as_ref()
            .and_then(|a| layout::parse_version(&a.version))
            .filter(|v| *v > self.image);
        let Some(version) = target else {
            self.newer_image_wins();
            return own(notices);
        };
        let replaced = active
            .as_ref()
            .and_then(|a| layout::parse_version(&a.previous));
        if replaced.is_some_and(|p| version <= p) {
            say!(
                "active names {version}, which isn't newer than the version it replaced; \
                 running {} instead",
                self.image
            );
            notices.push("bad_overlay".into());
            return own(notices);
        }
        match overlay::verify(&self.bin, &version) {
            Ok(v) => {
                if v.overlay.revoked_later {
                    notices.push("revoked_key".into());
                }
                if let Some(why) = &v.unsealed {
                    say!("running {version} from its file, without a sealed copy ({why})");
                }
                Plan {
                    program: v.program,
                    version,
                    _file: Some(v.file),
                    notices,
                }
            }
            Err(Refusal::Revoked) => {
                say!(
                    "{version} is signed by a key this build revokes; running {} instead",
                    self.image
                );
                notices.push("revoked_overlay".into());
                own(notices)
            }
            Err(Refusal::Bad(why)) => {
                say!(
                    "{version} failed verification ({why}); running {} instead. If the store was \
                     upgraded past what this version reads, pull the current image.",
                    self.image
                );
                let dir = self.bin.overlay(&version);
                let _ = std::fs::rename(&dir, self.bin.path(&format!("{version}.bad")));
                notices.push("bad_overlay".into());
                own(notices)
            }
            Err(Refusal::Unavailable(why)) => {
                say!(
                    "{version} can't be checked right now ({why}); running {} this time and \
                     trying {version} again at the next start",
                    self.image
                );
                notices.push("overlay_unavailable".into());
                own(notices)
            }
        }
    }

    /// With no newer active overlay, and nothing in flight, `active` and
    /// the overlays this build is at least as new as go.
    fn newer_image_wins(&self) {
        if self.bin.exists(journal::FILE) {
            return;
        }
        if pending::read(&self.bin)
            .ok()
            .flatten()
            .is_some_and(|p| p.version == self.image.to_string())
        {
            return;
        }
        let _ = self.bin.remove("pending");
        let _ = self.bin.remove("active");
        for v in self.bin.overlays().unwrap_or_default() {
            if v <= self.image {
                let _ = self.bin.remove_overlay(&v);
            }
        }
    }

    /// Charges an attempt in `pending` before a start in probation.
    fn charge(&self, version: &Version) -> Option<Pending> {
        // Damage was repaired at the top of the loop.
        let mut p = pending::read(&self.bin).ok().flatten()?;
        if p.version != version.to_string() {
            return None;
        }
        p.attempts += 1;
        if let Err(e) = self.bin.write("pending", &p) {
            // The attempt still counts in memory for this run; a restart
            // of the supervisor would see one fewer.
            say!("can't record the attempt in pending: {e}");
        }
        Some(p)
    }

    /// `pending` for `version` has used every attempt: begin its rollback
    /// (or halt a restored version).
    fn out_of_attempts(&self, version: &Version) -> Option<Next> {
        let p = pending::read(&self.bin).ok().flatten()?;
        if p.version != version.to_string() || p.attempts < ATTEMPTS {
            return None;
        }
        Some(self.give_up(&p))
    }

    fn spawn(&self, plan: &Plan, run_id: &str) -> std::io::Result<Pid> {
        let hint = Hint {
            pid: std::process::id(),
            run_id: run_id.to_string(),
            version: plan.version.to_string(),
            image_version: self.image.to_string(),
            notices: plan.notices.clone(),
        };
        // `ps` shows `lodestar`, not `/proc/self/fd/<n>`.
        let child = std::process::Command::new(&plan.program)
            .arg0("lodestar")
            .args(&self.args)
            .env(control::ENV, hint.encode())
            .spawn()?;
        // Never `Child::wait`: the reaper collects it. Dropping the handle
        // leaves the process running.
        let raw = child.id() as i32;
        drop(child);
        Pid::from_raw(raw).ok_or_else(|| std::io::Error::other("no child process id"))
    }

    /// Waits for the child to exit, forwarding signals, reaping, and
    /// judging probation every second on the supervisor's own clock. A
    /// forwarded stop that the child outlasts by the grace period becomes
    /// SIGKILL.
    async fn watch(
        &mut self,
        pid: Pid,
        run_id: &str,
        pending: Option<&Pending>,
        signals: &mut Signals,
    ) -> Ended {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut last = Instant::now();
        let mut clock = Clock {
            used: Duration::from_millis(pending.map_or(0, |p| p.used_ms)),
            ..Clock::default()
        };
        // The last beat seen (its value, and when it was first seen), and
        // when this attempt was first seen migrating.
        let mut beat: Option<(u64, Instant)> = None;
        let mut migrating_since: Option<Instant> = None;
        let mut kill_at: Option<Instant> = None;
        let mut ended = Ended {
            status: 0,
            forwarded: false,
            failed: None,
            passed: pending.is_none(),
        };
        loop {
            let forward = tokio::select! {
                _ = signals.term.recv() => Some(Signal::TERM),
                _ = signals.int.recv() => Some(Signal::INT),
                _ = signals.quit.recv() => Some(Signal::QUIT),
                _ = signals.hup.recv() => Some(Signal::HUP),
                _ = signals.usr1.recv() => Some(Signal::USR1),
                _ = signals.usr2.recv() => Some(Signal::USR2),
                _ = signals.chld.recv() => None,
                _ = tick.tick() => None,
            };
            if let Some(sig) = forward {
                if matches!(sig, Signal::TERM | Signal::INT | Signal::QUIT) {
                    ended.forwarded = true;
                    self.stopping = true;
                    kill_at.get_or_insert(Instant::now() + self.limits.grace);
                }
                let _ = rustix::process::kill_process(pid, sig);
            }
            if let Some(status) = reap(pid) {
                ended.status = status;
                return self.settle_time(ended, pending, clock.starting);
            }
            if kill_at.is_some_and(|t| Instant::now() >= t) {
                say!(
                    "the child didn't stop within {} s: killing it",
                    self.limits.grace.as_secs()
                );
                let _ = rustix::process::kill_process(pid, Signal::KILL);
                kill_at = None;
            }
            if ended.passed || ended.forwarded {
                continue;
            }
            let probation: Option<Probation> = self.bin.read("probation.json").ok().flatten();
            let now = Instant::now();
            let elapsed = now.saturating_duration_since(last);
            last = now;
            match probation
                .as_ref()
                .filter(|p| p.run_id == run_id && p.phase == pending::Phase::Migrating)
            {
                Some(p) => {
                    let since = *migrating_since.get_or_insert(now);
                    if beat.is_none_or(|(at, _)| at != p.at) {
                        beat = Some((p.at, now));
                    }
                    clock.since_beat = beat.map_or(Duration::ZERO, |(_, t)| now - t);
                    clock.migrating = now - since;
                }
                None => clock.starting += elapsed,
            }
            match pending::judge(
                self.bin.exists("pending"),
                probation.as_ref(),
                run_id,
                &clock,
                &self.limits,
            ) {
                Verdict::Wait => {}
                Verdict::Passed => {
                    ended.passed = true;
                    say!("the new version passed its self-test");
                }
                Verdict::Fail(why) => {
                    say!("the new version failed its probation: {why}");
                    ended.failed = Some(why);
                    ended.status = self.stop_child(pid, signals).await;
                    return self.settle_time(ended, pending, clock.starting);
                }
            }
        }
    }

    /// Records the time spent starting in `pending` (now that no child
    /// runs).
    fn settle_time(&self, ended: Ended, pending: Option<&Pending>, starting: Duration) -> Ended {
        if let (Some(p), false) = (pending, ended.passed) {
            if let Ok(Some(mut now)) = pending::read(&self.bin) {
                if now.version == p.version {
                    let ms = u64::try_from(starting.as_millis()).unwrap_or(u64::MAX);
                    now.used_ms = now.used_ms.saturating_add(ms);
                    let _ = self.bin.write("pending", &now);
                }
            }
        }
        ended
    }

    /// Probation passed (applied once the child has exited, since the
    /// supervisor writes only while no child runs): keep `active` and its
    /// previous overlay; the older overlays go.
    fn passed(&self) {
        let active: Option<Active> = self.bin.read("active").ok().flatten();
        let keep: Vec<String> = active
            .map(|a| vec![a.version, a.previous])
            .unwrap_or_default();
        let newest_kept = keep.iter().filter_map(|v| layout::parse_version(v)).min();
        for v in self.bin.overlays().unwrap_or_default() {
            if !keep.contains(&v.to_string()) && newest_kept.as_ref().is_some_and(|k| v < *k) {
                let _ = self.bin.remove_overlay(&v);
            }
        }
    }

    /// SIGTERM, the grace period, then SIGKILL; the child's status.
    /// A stop signal meanwhile is remembered: the supervisor then exits
    /// instead of restarting or rolling back.
    async fn stop_child(&mut self, pid: Pid, signals: &mut Signals) -> i32 {
        let _ = rustix::process::kill_process(pid, Signal::TERM);
        let until = Instant::now() + self.limits.grace;
        while Instant::now() < until {
            if let Some(s) = reap(pid) {
                return s;
            }
            tokio::select! {
                _ = signals.term.recv() => self.stopping = true,
                _ = signals.int.recv() => self.stopping = true,
                _ = signals.quit.recv() => self.stopping = true,
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
        let _ = rustix::process::kill_process(pid, Signal::KILL);
        loop {
            if let Some(s) = reap(pid) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// What happens after the child ended.
    fn after(
        &mut self,
        running: &Version,
        pending: Option<Pending>,
        ended: Ended,
        request: Option<control::Request>,
        notices: &mut Vec<String>,
    ) -> Next {
        let in_probation = pending.is_some() && !ended.passed && self.bin.exists("pending");
        if ended.forwarded {
            if in_probation {
                self.refund();
            }
            // It shut down cleanly on a request as the stop arrived: the
            // stop wins, and the shutdown was a clean one.
            if request.is_some() {
                return Next::Exit(ExitCode::SUCCESS);
            }
            return Next::Exit(exit_code(ended.status));
        }
        // Probation stopped it just as it passed: it's kept, so run it
        // again rather than exit with the status the stop caused.
        if ended.failed.is_some() && ended.passed {
            return Next::Again;
        }
        match request.as_ref().map(|r| r.action) {
            Some(Action::Restart) => {
                if in_probation {
                    self.refund();
                }
                return Next::Again;
            }
            Some(Action::Rollback) => {
                // Asked for, not a failure: if it's refused, the attempt
                // isn't spent.
                if in_probation {
                    self.refund();
                }
                let lost = request.and_then(|r| r.lost_torrents);
                let before = pending.filter(|_| in_probation).map(|p| p.index_before);
                return self.rollback(running, lost, before);
            }
            Some(Action::Switch) if !in_probation => {
                let v = request
                    .and_then(|r| r.version)
                    .and_then(|v| layout::parse_version(&v));
                if let Some(v) = v {
                    self.switch(running, &v, notices);
                }
                return Next::Again;
            }
            _ => {}
        }
        let Some(p) = pending.filter(|_| in_probation) else {
            return Next::Exit(exit_code(ended.status));
        };
        if ended.failed.is_none() {
            say!(
                "{} exited with status {} during its probation",
                p.version,
                ended.status
            );
        }
        if p.attempts < ATTEMPTS {
            return Next::Again;
        }
        self.give_up(&p)
    }

    /// Every attempt failed: roll an install back, halt a restored version.
    fn give_up(&self, p: &Pending) -> Next {
        match p.kind {
            Kind::Install => {
                say!(
                    "{} failed {ATTEMPTS} times: rolling back to {}",
                    p.version,
                    p.previous
                );
                let lost = self
                    .bin
                    .read::<Probation>("probation.json")
                    .ok()
                    .flatten()
                    .and_then(|pr| pr.stored_since);
                Next::Rollback(Rollback {
                    failed: p.version.clone(),
                    previous: p.previous.clone(),
                    reason: "failed",
                    index_before: Some(p.index_before.clone()),
                    lost,
                })
            }
            Kind::Restored => {
                let halted = Halted {
                    failed: p.previous.clone(),
                    previous: p.version.clone(),
                    at: crate::unix_now().max(0) as u64,
                    refused: None,
                };
                let _ = self.bin.write("halted.json", &halted);
                let _ = self.bin.remove("pending");
                self.honour_halt();
                Next::Exit(ExitCode::FAILURE)
            }
        }
    }

    /// Refunds the attempt charged for a run that ended in a forwarded stop
    /// or a supervised restart.
    fn refund(&self) {
        if let Ok(Some(mut p)) = pending::read(&self.bin) {
            p.attempts = p.attempts.saturating_sub(1);
            let _ = self.bin.write("pending", &p);
        }
    }

    /// `EXIT_SWITCH`: only to a newer version that isn't skipped (an
    /// unreadable `skipped` refuses), verified; then `pending`, then
    /// `active`. If `pending` can't be written, nothing switches.
    fn switch(&self, running: &Version, to: &Version, notices: &mut Vec<String>) {
        if to <= running {
            say!("{to} isn't newer than {running}; versions only go up");
            return;
        }
        match pending::is_skipped(&self.bin, &to.to_string()) {
            Ok(false) => {}
            Ok(true) => {
                say!(
                    "{to} was rolled back before and is never installed again; staying on {running}"
                );
                return;
            }
            Err(e) => {
                say!("reading skipped: {e}; staying on {running}");
                return;
            }
        }
        if overlay::verify(&self.bin, to).is_err() {
            say!("{to} failed verification; staying on {running}");
            notices.push("bad_overlay".into());
            return;
        }
        let previous = if *running == self.image {
            pending::IMAGE.to_string()
        } else {
            running.to_string()
        };
        let p = Pending {
            format: pending::PENDING_FORMAT,
            version: to.to_string(),
            previous: previous.clone(),
            kind: Kind::Install,
            attempts: 0,
            used_ms: 0,
            index_before: journal::list_index(&self.index_root),
        };
        if let Err(e) = self.bin.write("pending", &p) {
            say!("can't write pending ({e}): staying on {running}");
            notices.push("pending_unwritable".into());
            return;
        }
        let active = Active {
            version: to.to_string(),
            previous,
        };
        if let Err(e) = self.bin.write("active", &active) {
            say!("can't write active ({e}): staying on {running}");
            let _ = self.bin.remove("pending");
            notices.push("pending_unwritable".into());
            return;
        }
        say!("switching from {running} to {to}");
    }

    /// `EXIT_ROLLBACK`: the running overlay back to its previous version.
    fn rollback(
        &mut self,
        running: &Version,
        lost: Option<u64>,
        index_before: Option<Vec<String>>,
    ) -> Next {
        let Some(active) = self.bin.read::<Active>("active").ok().flatten() else {
            say!("nothing to roll back to: {running} is this image's own version");
            return Next::Again;
        };
        if active.version != running.to_string() {
            return Next::Again;
        }
        Next::Rollback(Rollback {
            failed: active.version,
            previous: active.previous,
            reason: "manual",
            index_before,
            lost,
        })
    }
}

/// `lodestar rollback --update` (server stopped): takes the lock, then
/// finishes a journal in progress or rolls the active overlay back to its
/// previous version (reason `manual`, lost torrents unknown). It starts
/// nothing.
pub fn rollback_offline(
    config: &crate::config::Config,
    confirm: impl FnOnce(&str) -> crate::cli::util::Outcome<bool>,
) -> crate::cli::util::Outcome {
    use crate::cli::util::Failure;
    let bin = Bin::existing(&config.data_dir)
        .map_err(|e| Failure::Failed(e.to_string()))?
        .ok_or_else(|| Failure::Failed("there's no self-installed update to roll back".into()))?;
    let _lock = bin
        .lock()
        .map_err(|e| Failure::Failed(e.to_string()))?
        .ok_or_else(|| Failure::Refused("Lodestar is running: stop it first".into()))?;
    let ctx = Ctx {
        bin: bin.clone(),
        db: config.db_path(),
        index_root: config.data_dir.join("index"),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        image_version: build_version().to_string(),
    };
    let j = match journal::current(&bin).map_err(|e| Failure::Failed(e.to_string()))? {
        Some(j) => j,
        None => {
            let active: Active = bin
                .read("active")
                .map_err(|e| Failure::Failed(e.to_string()))?
                .ok_or_else(|| {
                    Failure::Failed("there's no self-installed update to roll back".into())
                })?;
            let image = build_version();
            if layout::parse_version(&active.version).is_none_or(|v| v <= image) {
                return Err(Failure::Failed(format!(
                    "there's no self-installed update to roll back: {} isn't newer than this \
                     image's {image}",
                    active.version
                )));
            }
            let preview = journal::preview(&ctx, &active.version, &active.previous)
                .map_err(Failure::Refused)?;
            let restores = match preview {
                Some(snap) => format!(
                    "the store is restored from {snap}: torrents stored since {} was installed \
                     are lost",
                    active.version
                ),
                None => "the store is kept as it is".to_string(),
            };
            let summary = format!(
                "Roll {} back to {}: {restores}. {} is never installed again.",
                active.version, active.previous, active.version
            );
            if !confirm(&summary)? {
                println!("Nothing was changed.");
                return Ok(());
            }
            // In probation, its own record of `index/` before it counts.
            let index_before = pending::read(&bin)
                .ok()
                .flatten()
                .filter(|p| p.version == active.version)
                .map(|p| p.index_before);
            let now = crate::unix_now().max(0) as u64;
            journal::begin(
                &ctx,
                &active.version,
                &active.previous,
                "manual",
                index_before.as_deref(),
                None,
                now,
            )
            .map_err(|e| match e {
                NotBegun::Refused(why) => Failure::Refused(why),
                e => Failure::Failed(e.to_string()),
            })?
        }
    };
    let (failed, previous) = (j.failed.clone(), j.previous.clone());
    journal::finish(&ctx, j).map_err(|e| match e {
        Unfinished::Stopped => Failure::Failed("stopped; run it again to finish".into()),
        Unfinished::Failed(why) => Failure::Failed(why),
    })?;
    println!("Rolled {failed} back to {previous}. It starts the next time Lodestar does.");
    Ok(())
}

enum Next {
    Again,
    Exit(ExitCode),
    /// Begin this rollback, then go round again to finish it.
    Rollback(Rollback),
}

#[derive(Debug, Clone)]
struct Rollback {
    failed: String,
    previous: String,
    /// `failed` (every attempt failed: a refusal halts) or `manual`.
    reason: &'static str,
    index_before: Option<Vec<String>>,
    lost: Option<u64>,
}

/// Reaps every exited child; the status of `ours` if it was one.
fn reap(ours: Pid) -> Option<i32> {
    let mut found = None;
    while let Ok(Some((pid, status))) = rustix::process::wait(WaitOptions::NOHANG) {
        if pid == ours {
            found = Some(
                status
                    .exit_status()
                    .or_else(|| status.terminating_signal().map(|s| 128 + s))
                    .unwrap_or(1),
            );
        }
    }
    found
}

fn exit_code(status: i32) -> ExitCode {
    ExitCode::from(u8::try_from(status).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_child_runs_serve_with_child_and_the_config() {
        assert_eq!(child_args(None), ["serve", "--child"]);
        assert_eq!(
            child_args(Some(Path::new("c.toml"))),
            ["--config", "c.toml", "serve", "--child"]
        );
    }
}
