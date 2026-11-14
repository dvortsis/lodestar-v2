//! The companion supervisor (plan `docs/plan/2026-11-06-v06-companions.md`
//! §4 CMP-4a; Q3, Q7; architecture §18.10).
//!
//! - [`process`]: starting one companion with a minimal environment, in its
//!   own folder, never elevated, with CMP-4b's [`AfterSpawn`] hook.
//! - [`logs`]: each companion's 1 MB output ring, redacted before a line
//!   enters it.
//! - [`pidfile`]: records of running companions, and killing orphans at
//!   start-up and from the init.
//!
//! [`Supervisor`] runs one task per companion. Each task restarts its
//! companion with Q7's backoff, checks its health through CMP-0's
//! [`Health`] trait, and publishes its [`Status`] on a watch channel (for
//! CMP-5 and CMP-6). After [`Timings::crash_limit`] crashes inside
//! [`Timings::crash_window`] it stops restarting and shows
//! [`State::NeedsAttention`] until [`Supervisor::restart`]. Health failures
//! that restart a companion count as crashes. A run that lasted
//! [`Timings::backoff_cap`] or longer resets the backoff.
//!
//! **Concurrency.** The only locks are the log ring's, held to push or copy
//! a line. A task owns its child outright; the child is never shared, so
//! nothing waits on it under a lock. Every child is spawned with
//! `kill_on_drop`, and dropping the [`Supervisor`] aborts its tasks, so a
//! cancelled supervisor leaves no companion or task behind. Children are
//! only ever waited on by pid (Tokio's `Child`), never with a wait-any, so
//! this never races the init's reaper; the supervisor runs in the work
//! child, not in the init.
//!
//! **Shutdown** ([`Supervisor::shutdown`]) stops companions one at a time
//! in reverse order of [`Supervisor::add`] (a companion is added after the
//! ones it depends on): SIGTERM on Unix and the companion's [`Stop`]
//! request if it has one, [`Timings::stop_deadline`], then a kill. Each
//! step is bounded, so the whole is bounded.

pub mod bind;
pub mod egress;
pub mod limits;
#[cfg(target_os = "linux")]
pub mod limits_linux;
#[cfg(windows)]
pub mod limits_windows;
pub mod logs;
pub mod pidfile;
pub mod process;

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use lodestar_integrations::secret::Secret;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::adapter::{BoxFuture, GuardedHttp, Health, HealthStatus, Target};
use crate::inputs::CompanionId;

pub use limits::{Confinement, Launcher, Limits, Report, Sandbox};
pub use logs::LogRing;
pub use process::{AfterSpawn, EnvName, NoHook, SpawnError, SpawnSpec};

/// How long a killed companion may take to be reaped before its task gives
/// up waiting (a process stuck in the kernel). Bounds shutdown.
const KILL_WAIT: Duration = Duration::from_secs(5);
/// How long the output readers may outlive the companion (a grandchild
/// holding its pipes) before they're aborted.
const READER_WAIT: Duration = Duration::from_secs(1);

/// The supervision numbers. [`Timings::default`] is plan Q7's, which every
/// shipped caller uses; other values are for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    /// The first restart delay; it doubles per crash in a row.
    pub backoff_start: Duration,
    pub backoff_cap: Duration,
    pub crash_window: Duration,
    /// This many crashes inside `crash_window` stop restarts.
    pub crash_limit: usize,
    pub health_every: Duration,
    /// This many failed health checks in a row restart it.
    pub health_failures: u32,
    pub stop_deadline: Duration,
}

impl Default for Timings {
    fn default() -> Timings {
        Timings {
            backoff_start: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(5 * 60),
            crash_window: Duration::from_secs(10 * 60),
            crash_limit: 5,
            health_every: Duration::from_secs(30),
            health_failures: 3,
            stop_deadline: Duration::from_secs(10),
        }
    }
}

/// The delay before restart number `crashes_in_a_row` (1-based): the start
/// value doubled each time, capped.
pub fn backoff(crashes_in_a_row: u32, t: &Timings) -> Duration {
    let doublings = crashes_in_a_row.saturating_sub(1).min(31);
    t.backoff_start
        .saturating_mul(1u32 << doublings)
        .min(t.backoff_cap)
}

/// Why a companion waits for you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    /// It crashed [`Timings::crash_limit`] times inside the window.
    CrashLoop,
    /// It can't be started as configured; the [`SpawnError`] code.
    Refused(&'static str),
}

impl Attention {
    pub fn code(&self) -> &'static str {
        match self {
            Attention::CrashLoop => "crash_loop",
            Attention::Refused(code) => code,
        }
    }
}

/// Where a companion is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Starting,
    Running,
    /// Running, with this many failed health checks in a row.
    Unhealthy {
        failures: u32,
    },
    /// Waiting this long before the next start.
    BackingOff {
        delay: Duration,
    },
    /// Not restarting until [`Supervisor::restart`].
    NeedsAttention(Attention),
    /// Stopped and kept stopped by a hold (CMP-4b's route
    /// gate: its route is down) until released. Not a crash.
    Held,
    Stopped,
}

impl State {
    pub fn code(&self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Unhealthy { .. } => "unhealthy",
            State::BackingOff { .. } => "backing_off",
            State::NeedsAttention(_) => "needs_attention",
            State::Held => "held",
            State::Stopped => "stopped",
        }
    }
}

/// What a companion's watch channel carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    /// Unplanned exits and health restarts since it was added.
    pub restarts: u32,
    /// How its last run ended: its exit code, or 128 + the signal.
    pub last_exit: Option<i32>,
    /// Which run of it this is: one more at each start of the process, so
    /// a restart is told apart from the run before (`bind`).
    pub run: u64,
}

/// A companion's health check: the adapter (CMP-8a), the guarded client
/// and where it listens.
#[derive(Clone)]
pub struct Probe {
    pub health: Arc<dyn Health>,
    pub http: Arc<dyn GuardedHttp>,
    pub target: Target,
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probe")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// A graceful stop request through the companion's own API (an adapter's
/// shutdown call). On Windows, without one, a companion is killed at once.
pub trait Stop: Send + Sync {
    fn request(&self) -> BoxFuture<'_, ()>;
}

/// One companion to supervise.
pub struct Companion {
    pub spawn: SpawnSpec,
    /// Companions that must already be added; shutdown stops this one
    /// first.
    pub depends_on: Vec<CompanionId>,
    pub probe: Option<Probe>,
    /// Every credential CMP-3 generated or you pasted for it, redacted
    /// from its output.
    pub secrets: Vec<Secret>,
    pub stop: Option<Arc<dyn Stop>>,
}

impl std::fmt::Debug for Companion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Companion")
            .field("id", &self.spawn.id)
            .field("depends_on", &self.depends_on)
            .field("probe", &self.probe)
            .finish_non_exhaustive()
    }
}

/// Why a companion wasn't added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SuperviseError {
    /// It's already supervised.
    Duplicate,
    /// Its id is `run`, the records folder's name.
    ReservedId,
    /// A companion it depends on hasn't been added.
    MissingDependency,
    /// Its data folder isn't the supervisor's.
    OtherDataDir,
    /// The orphan cleanup at start couldn't read the records.
    Records(pidfile::PidfileError),
}

impl SuperviseError {
    pub fn code(&self) -> &'static str {
        match self {
            SuperviseError::Duplicate => "duplicate",
            SuperviseError::ReservedId => "reserved_id",
            SuperviseError::MissingDependency => "missing_dependency",
            SuperviseError::OtherDataDir => "other_data_dir",
            SuperviseError::Records(e) => e.code(),
        }
    }
}

impl std::fmt::Display for SuperviseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "companion supervisor: {}", self.code())
    }
}

impl std::error::Error for SuperviseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    /// Start it again now (clears Needs attention and the backoff).
    Restart,
    Stop,
    /// Stop it and keep it stopped until [`Control::Release`].
    Hold,
    Release,
}

/// Holds and releases one companion: CMP-4b's route gate. A held
/// companion is stopped and doesn't start or restart until released; the
/// stop isn't a crash.
#[derive(Debug, Clone)]
pub(crate) struct Handle {
    control: mpsc::UnboundedSender<Control>,
}

impl Handle {
    /// `false` once the companion is no longer supervised.
    pub(crate) fn hold(&self) -> bool {
        self.control.send(Control::Hold).is_ok()
    }

    /// Lets it start again. One that needed your attention before the
    /// hold still does: only [`Supervisor::restart`] clears that.
    pub(crate) fn release(&self) -> bool {
        self.control.send(Control::Release).is_ok()
    }
}

struct Supervised {
    id: CompanionId,
    status: watch::Receiver<Status>,
    logs: LogRing,
    control: mpsc::UnboundedSender<Control>,
    task: JoinHandle<()>,
}

/// Supervises companions. Dropping it aborts every task and kills every
/// companion; [`Supervisor::shutdown`] stops them in order first.
pub struct Supervisor {
    data_dir: PathBuf,
    hook: Arc<dyn AfterSpawn>,
    timings: Timings,
    running: Vec<Supervised>,
    /// What the orphan cleanup did at start.
    pub orphans: pidfile::Cleanup,
}

impl Supervisor {
    /// Kills orphans recorded by an earlier run (pidfile), before anything
    /// starts. One supervisor per data folder: a second one would kill the
    /// first one's companions as orphans.
    pub async fn new(
        data_dir: PathBuf,
        hook: Arc<dyn AfterSpawn>,
        timings: Timings,
    ) -> Result<Supervisor, SuperviseError> {
        let dir = data_dir.clone();
        // Blocking: on Windows each identity check runs PowerShell.
        let orphans = tokio::task::spawn_blocking(move || pidfile::kill_orphans(&dir))
            .await
            .unwrap_or(Err(pidfile::PidfileError::Io(std::io::ErrorKind::Other)))
            .map_err(SuperviseError::Records)?;
        Ok(Supervisor {
            data_dir,
            hook,
            timings,
            running: Vec::new(),
            orphans,
        })
    }

    /// Starts supervising `c`. Everything it depends on must be added
    /// first.
    pub fn add(&mut self, c: Companion) -> Result<(), SuperviseError> {
        self.insert(c, false)
    }

    /// Like [`Supervisor::add`], but held: it doesn't start until the route
    /// gate releases it.
    pub(crate) fn add_held(&mut self, c: Companion) -> Result<(), SuperviseError> {
        self.insert(c, true)
    }

    fn insert(&mut self, c: Companion, held: bool) -> Result<(), SuperviseError> {
        let id = c.spawn.id.clone();
        if self.find(&id).is_some() {
            return Err(SuperviseError::Duplicate);
        }
        if process::is_reserved(&id) {
            return Err(SuperviseError::ReservedId);
        }
        if c.depends_on.iter().any(|d| self.find(d).is_none()) {
            return Err(SuperviseError::MissingDependency);
        }
        if c.spawn.data_dir != self.data_dir {
            return Err(SuperviseError::OtherDataDir);
        }
        let (status_tx, status) = watch::channel(Status {
            state: if held { State::Held } else { State::Starting },
            restarts: 0,
            last_exit: None,
            run: 0,
        });
        let (control, control_rx) = mpsc::unbounded_channel();
        let logs = LogRing::default();
        let task = tokio::spawn(supervise(
            c,
            self.hook.clone(),
            self.timings,
            status_tx,
            logs.clone(),
            control_rx,
            held,
        ));
        self.running.push(Supervised {
            id,
            status,
            logs,
            control,
            task,
        });
        Ok(())
    }

    fn find(&self, id: &CompanionId) -> Option<&Supervised> {
        self.running.iter().find(|s| &s.id == id)
    }

    /// The companion's status channel.
    pub fn status(&self, id: &CompanionId) -> Option<watch::Receiver<Status>> {
        self.find(id).map(|s| s.status.clone())
    }

    /// Its hold and release: the route gate's alone, so nothing outside the
    /// crate can start a gated companion while its route is down.
    pub(crate) fn handle(&self, id: &CompanionId) -> Option<Handle> {
        self.find(id).map(|s| Handle {
            control: s.control.clone(),
        })
    }

    /// The companion's output ring.
    pub fn logs(&self, id: &CompanionId) -> Option<LogRing> {
        self.find(id).map(|s| s.logs.clone())
    }

    /// Restarts it now: what you do after Needs attention. `false` if it
    /// isn't supervised.
    pub fn restart(&self, id: &CompanionId) -> bool {
        self.find(id)
            .is_some_and(|s| s.control.send(Control::Restart).is_ok())
    }

    /// Stops and forgets one companion. Bounded like [`Supervisor::shutdown`].
    /// It doesn't look at dependents: remove the companions that depend on
    /// it first.
    pub async fn remove(&mut self, id: &CompanionId) {
        if let Some(i) = self.running.iter().position(|s| &s.id == id) {
            let s = self.running.remove(i);
            stop_task(s, &self.timings).await;
        }
    }

    /// Stops every companion, in reverse order of [`Supervisor::add`].
    pub async fn shutdown(mut self) {
        while let Some(s) = self.running.pop() {
            stop_task(s, &self.timings).await;
        }
    }
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<&str> = self.running.iter().map(|s| s.id.as_str()).collect();
        f.debug_struct("Supervisor")
            .field("companions", &ids)
            .finish_non_exhaustive()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        for s in &self.running {
            s.task.abort();
        }
    }
}

/// Asks one task to stop and waits for it, bounded; aborts it (killing the
/// companion) if it overruns.
async fn stop_task(mut s: Supervised, t: &Timings) {
    let _ = s.control.send(Control::Stop);
    let bound = t.stop_deadline + KILL_WAIT + READER_WAIT + Duration::from_secs(1);
    if tokio::time::timeout(bound, &mut s.task).await.is_err() {
        s.task.abort();
        let _ = s.task.await;
    }
}

/// How one run ended.
enum Ended {
    /// It exited or was restarted for its health.
    Crashed {
        ran: Duration,
    },
    /// You asked for a restart.
    Restart,
    /// It was held.
    Held,
    Stopped,
}

/// One companion's task.
async fn supervise(
    c: Companion,
    hook: Arc<dyn AfterSpawn>,
    t: Timings,
    status: watch::Sender<Status>,
    logs: LogRing,
    mut control: mpsc::UnboundedReceiver<Control>,
    mut held: bool,
) {
    let redactor = Arc::new(logs::Redactor::new(&c.secrets));
    let set = |state: State| status.send_modify(|s| s.state = state);
    let mut crashes: VecDeque<Instant> = VecDeque::new();
    let mut in_a_row = 0u32;
    // Needs attention survives a hold: a route that comes back doesn't
    // clear it, only your restart does.
    let mut parked: Option<Attention> = None;
    loop {
        if held {
            set(State::Held);
            match control.recv().await.unwrap_or(Control::Stop) {
                Control::Stop => break,
                Control::Release => {
                    held = false;
                    // The route is back: start promptly, not after a backoff.
                    in_a_row = 0;
                }
                Control::Restart => {
                    parked = None;
                    crashes.clear();
                    in_a_row = 0;
                }
                Control::Hold => {}
            }
            continue;
        }
        if let Some(why) = parked {
            set(State::NeedsAttention(why));
            match wait_for_you(&mut control).await {
                Control::Restart => {
                    parked = None;
                    crashes.clear();
                    in_a_row = 0;
                }
                Control::Hold => held = true,
                Control::Stop | Control::Release => break,
            }
            continue;
        }
        // Commands that queued up during a stop (after a restart, say):
        // the last hold or release wins over the start it would race.
        let mut stop = false;
        while let Ok(cmd) = control.try_recv() {
            match cmd {
                Control::Hold => held = true,
                Control::Release => held = false,
                Control::Stop => {
                    stop = true;
                    break;
                }
                Control::Restart => {}
            }
        }
        // A stop, or nobody left to send one.
        if stop || (control.is_closed() && control.is_empty()) {
            break;
        }
        if held {
            continue;
        }
        set(State::Starting);
        let (spec, h) = (c.spawn.clone(), hook.clone());
        let spawned = tokio::task::spawn_blocking(move || process::spawn(&spec, h.as_ref()))
            .await
            .unwrap_or(Err(SpawnError::Start(std::io::ErrorKind::Other)));
        let ended = match spawned {
            Ok(started) => run(&c, started, &t, &status, &logs, &redactor, &mut control).await,
            Err(
                e @ (SpawnError::Privileged
                | SpawnError::PrivilegeUnknown
                | SpawnError::BadExecutable
                | SpawnError::BadEnvName
                | SpawnError::ReservedId),
            ) => {
                parked = Some(Attention::Refused(e.code()));
                continue;
            }
            Err(_) => Ended::Crashed {
                ran: Duration::ZERO,
            },
        };
        match ended {
            Ended::Stopped => break,
            Ended::Held => held = true,
            Ended::Restart => {
                crashes.clear();
                in_a_row = 0;
            }
            Ended::Crashed { ran } => {
                status.send_modify(|s| s.restarts = s.restarts.saturating_add(1));
                if ran >= t.backoff_cap {
                    in_a_row = 0;
                }
                in_a_row = in_a_row.saturating_add(1);
                let now = Instant::now();
                crashes.push_back(now);
                while crashes
                    .front()
                    .is_some_and(|at| now.duration_since(*at) >= t.crash_window)
                {
                    crashes.pop_front();
                }
                if crashes.len() >= t.crash_limit {
                    parked = Some(Attention::CrashLoop);
                    continue;
                }
                let delay = backoff(in_a_row, &t);
                set(State::BackingOff { delay });
                let next = tokio::select! {
                    () = tokio::time::sleep(delay) => continue,
                    c = wait_for_you(&mut control) => c,
                };
                match next {
                    Control::Stop | Control::Release => break,
                    Control::Hold => held = true,
                    Control::Restart => {
                        crashes.clear();
                        in_a_row = 0;
                    }
                }
            }
        }
    }
    set(State::Stopped);
}

/// Waits for a restart, stop or hold; a closed channel is a stop. A
/// release means nothing to a companion that isn't held, so it's skipped
/// (never returned).
async fn wait_for_you(control: &mut mpsc::UnboundedReceiver<Control>) -> Control {
    loop {
        match control.recv().await {
            Some(Control::Release) => continue,
            Some(c) => return c,
            None => return Control::Stop,
        }
    }
}

type Check<'a> = Pin<Box<dyn Future<Output = Option<HealthStatus>> + Send + 'a>>;

/// Supervises one run of the companion until it exits, is restarted or
/// stopped.
async fn run(
    c: &Companion,
    started: process::Started,
    t: &Timings,
    status: &watch::Sender<Status>,
    ring: &LogRing,
    redactor: &Arc<logs::Redactor>,
    control: &mut mpsc::UnboundedReceiver<Control>,
) -> Ended {
    let process::Started { mut child, pid } = started;
    // Whatever the companion started is killed once it has gone (below),
    // or when this future is dropped: `group` is declared after `child`,
    // so it drops first, while the leader is still unreaped and holds the
    // group's id.
    let mut group = Group(Some(pid));
    let mut readers = Readers(Vec::new());
    if let Some(out) = child.stdout.take() {
        readers
            .0
            .push(tokio::spawn(read_into(out, ring.clone(), redactor.clone())));
    }
    if let Some(err) = child.stderr.take() {
        readers
            .0
            .push(tokio::spawn(read_into(err, ring.clone(), redactor.clone())));
    }
    status.send_modify(|s| {
        s.state = State::Running;
        s.run += 1;
    });
    let began = Instant::now();
    let mut tick = tokio::time::interval_at(began + t.health_every, t.health_every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failures = 0u32;
    let mut check: Option<Check<'_>> = None;
    let (ended, exit) = loop {
        tokio::select! {
            r = child.wait() => {
                break (Ended::Crashed { ran: began.elapsed() }, r.ok().map(exit_code));
            }
            _ = tick.tick(), if c.probe.is_some() && check.is_none() => {
                check = c.probe.as_ref().map(|p| -> Check<'_> {
                    Box::pin(async move {
                        tokio::time::timeout(t.health_every, p.health.check(p.http.as_ref(), &p.target))
                            .await
                            .ok()
                    })
                });
            }
            r = async { match check.as_mut() { Some(f) => f.await, None => std::future::pending().await } } => {
                check = None;
                if r == Some(HealthStatus::Healthy) {
                    failures = 0;
                    // Watchers wake only on a change, not every check.
                    status.send_if_modified(|s| {
                        let changed = s.state != State::Running;
                        s.state = State::Running;
                        changed
                    });
                    continue;
                }
                failures += 1;
                status.send_modify(|s| s.state = State::Unhealthy { failures });
                if failures >= t.health_failures {
                    let exit = stop_child(&mut child, c.stop.as_deref(), t.stop_deadline).await;
                    break (Ended::Crashed { ran: began.elapsed() }, exit);
                }
            }
            cmd = control.recv() => {
                let ended = match cmd {
                    // It isn't held: nothing to release.
                    Some(Control::Release) => continue,
                    Some(Control::Restart) => Ended::Restart,
                    Some(Control::Hold) => Ended::Held,
                    Some(Control::Stop) | None => Ended::Stopped,
                };
                let exit = stop_child(&mut child, c.stop.as_deref(), t.stop_deadline).await;
                break (ended, exit);
            }
        }
    };
    drop(check);
    // At once, right after the leader's wait: what it started goes with it.
    group.kill();
    status.send_modify(|s| s.last_exit = exit);
    // Reaped: its pid isn't ours any more. One that didn't die within the
    // kill wait keeps its record, so the init's hook and the next start
    // still find it.
    if exit.is_some() {
        pidfile::remove(&c.spawn.data_dir, &c.spawn.id);
    }
    for r in &mut readers.0 {
        let _ = tokio::time::timeout(READER_WAIT, r).await;
    }
    ended
}

/// The output readers, aborted when dropped, so none outlives its run
/// even when the run itself is cancelled.
struct Readers(Vec<JoinHandle<()>>);

impl Drop for Readers {
    fn drop(&mut self) {
        for r in &self.0 {
            r.abort();
        }
    }
}

/// A companion's process group (Unix), killed when dropped: what it
/// started dies with it. The leader is reaped (or being killed) by then;
/// the group's id stays reserved while any member lives, so only members
/// can be hit.
struct Group(Option<u32>);

impl Group {
    /// SIGKILL to the group, once.
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.0.take() {
            signal_group(pgid, rustix::process::Signal::KILL);
        }
        #[cfg(not(unix))]
        self.0.take();
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(unix)]
fn signal_group(pgid: u32, sig: rustix::process::Signal) {
    if let Some(p) = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(p, sig);
    }
}

/// Stops the child: SIGTERM (Unix) and its [`Stop`] request, the deadline,
/// then a kill. Its exit code, if it was reaped.
async fn stop_child(
    child: &mut tokio::process::Child,
    stop: Option<&dyn Stop>,
    deadline: Duration,
) -> Option<i32> {
    // `id()` is `None` once Tokio has reaped it: no signal to a reused
    // pid. The leader's pid is its group's id.
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        signal_group(pid, rustix::process::Signal::TERM);
    }
    if cfg!(unix) || stop.is_some() {
        let graceful = async {
            if let Some(s) = stop {
                let _ = tokio::time::timeout(deadline, s.request()).await;
            }
            child.wait().await
        };
        if let Ok(r) = tokio::time::timeout(deadline, graceful).await {
            return r.ok().map(exit_code);
        }
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        signal_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.start_kill();
    match tokio::time::timeout(KILL_WAIT, child.wait()).await {
        Ok(r) => r.ok().map(exit_code),
        Err(_) => None,
    }
}

fn exit_code(s: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return 128 + sig;
        }
    }
    s.code().unwrap_or(-1)
}

/// Copies one stream into the ring, a line at a time.
async fn read_into(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    ring: LogRing,
    redactor: Arc<logs::Redactor>,
) {
    let mut lines = logs::Lines::new(ring, redactor);
    let mut buf = vec![0u8; 8 * 1024];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => lines.feed(&buf[..n]),
        }
    }
    lines.finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_one_second_to_five_minutes() {
        let t = Timings::default();
        let got: Vec<u64> = (1..=12).map(|n| backoff(n, &t).as_secs()).collect();
        assert_eq!(got, [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300, 300]);
        assert_eq!(backoff(u32::MAX, &t), t.backoff_cap);
        assert_eq!(backoff(0, &t), t.backoff_start);
    }

    #[test]
    fn the_defaults_are_q7s() {
        let t = Timings::default();
        assert_eq!(t.crash_limit, 5);
        assert_eq!(t.crash_window, Duration::from_secs(600));
        assert_eq!(t.health_every, Duration::from_secs(30));
        assert_eq!(t.health_failures, 3);
        assert_eq!(t.stop_deadline, Duration::from_secs(10));
    }

    #[test]
    fn codes_are_stable() {
        assert_eq!(
            State::BackingOff {
                delay: Duration::ZERO
            }
            .code(),
            "backing_off"
        );
        assert_eq!(
            State::NeedsAttention(Attention::CrashLoop).code(),
            "needs_attention"
        );
        assert_eq!(Attention::Refused("privileged").code(), "privileged");
    }
}
