//! The update engine (§12.2) in `serve --child`: the scheduled and manual
//! signed checks, `updater.json`, the policy over what a check found, and
//! everything `/updates` and `/system` report, and installs.
//!
//! - **When:** with `mode` notify, auto or image, at most every 6 hours
//!   at a random minute (`updater.json` keeps the last and next check);
//!   `POST /updates/check` at any mode, at most once a minute, one at a
//!   time.
//! - **The check:** [`check::fetch`], then `lodestar_update::check`
//!   against `trust.json`, raised to the floor (the embedded statements
//!   and every verified overlay's statements and marks; rebuilt from it
//!   when missing; a damaged one blocks installs and is never written),
//!   then the forward-only write of the next trust state.
//! - **Installing** (`POST /updates/install`, a version queued for the
//!   window, or the policy's next automatic install), holding the
//!   one-check lock throughout: the policy's Install now rules and the
//!   manual-steps confirmation; nothing else in the way (a rebuild) and
//!   room on the disk; the envelope verified again against the trust
//!   state and the clock now; [`download`]; [`install::stage`]; the staged
//!   folder verified as the supervisor will (`overlay::verify`); the
//!   envelope verified once more (§12.2 "At install time"); then the
//!   switch intent and the graceful shutdown. A failed install isn't
//!   retried until the next check.
//! - **Events:** `checking` when a check starts; `checked`, `available` or
//!   `failed` when it ends; `waiting_window`, `downloading` (progress at
//!   most once a second), `verifying`, `installing`, `restarting` or
//!   `failed` for an install. After a switch the new version publishes
//!   `checking_health`, then `done` when its self-test passes; a restored
//!   version publishes `rolled_back` once, from the history.
//! - **A failed check** keeps the last good check's releases: `/updates`
//!   shows them with `state: failed` and the error, so the screen says
//!   what's known and that the newest check failed. Nothing installs from
//!   a result older than its manifest's expiry.
//! - **Acknowledged means durable** (DUR-26): every change to the
//!   updater's state goes through one path under the `updater` mutex:
//!   start from the last durable state, apply the change, write it
//!   (temporary file, fsync, rename, folder fsync), and only then publish
//!   it and acknowledge. A pause, resume, skip or history clear that
//!   can't be written is [`UpdatesError::NotSaved`]; a failed pause also
//!   holds automatic installs in this process ([`policy::Block::Held`])
//!   until a pause or resume is saved, and is never written as a pause.
//!   A check whose times can't be written keeps them in memory only
//!   (`check_not_saved`), so they never overwrite a durable pause.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_api::dto::events::{Event, UpdatesEvent, UpdatesState};
use lodestar_api::events::EventHub;
use lodestar_api::ops::BoxFuture;
use lodestar_api::updates::{
    Blocked, Capability, CheckState, CheckStatus, HistoryEntry, InstallWhen, Installing,
    PendingInstall, Policy, ReleaseInfo, RollbackInfo, Running, UpdatesApi, UpdatesError,
    UpdatesStatus,
};
use lodestar_log::log_event;
use lodestar_update::semver::Version;
use lodestar_update::{Checked, Notice};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use super::check::{self, CheckError, Fetch};
use super::layout::{Bin, Step};
use super::log_events::{
    UPDATE_AVAILABLE, UPDATE_CHECK_FAILED, UPDATE_INSTALL_FAILED, UPDATE_OVERLAY_UNREADABLE,
    UPDATE_STATE_UNREADABLE, UPDATE_STATE_UNSAVED,
};
use super::policy::{self, Block, Reason, Refusal};
use super::window::{When, Window};
use super::{download, history, install, journal, layout, pending, trust_file};
use crate::settings::updates::{Mode, Updates};

/// Between scheduled checks, before the random minute.
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Between manual checks.
const MANUAL_EVERY: Duration = Duration::from_secs(60);
/// The longest the schedule sleeps before it looks at the clock again,
/// so a clock that's corrected while it sleeps is noticed.
const MAX_SLEEP: Duration = Duration::from_secs(15 * 60);
pub const UPDATER_FILE: &str = "updater.json";
/// Between two `downloading` events.
const PROGRESS_EVERY: Duration = Duration::from_secs(1);
/// Kept free on `<data>/bin`'s disk beyond what an install needs.
const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

/// `updater.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Updater {
    pub format: u32,
    #[serde(default)]
    pub checked_at: Option<i64>,
    #[serde(default)]
    pub next_check_at: Option<i64>,
    #[serde(default)]
    pub paused_until: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
    /// A version asked for at the next window (`POST /updates/install`
    /// with `when: window`).
    #[serde(default)]
    pub queued: Option<String>,
    /// The newest history entry a `rolled_back` event was published for.
    #[serde(default)]
    pub rolled_back_seen: Option<u64>,
}

/// The check's half of `updater.json`: what a check that couldn't be
/// saved keeps in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckTimes {
    checked_at: i64,
    next_check_at: i64,
    error: Option<String>,
}

impl CheckTimes {
    fn apply(&self, u: &mut Updater) {
        u.checked_at = Some(self.checked_at);
        u.next_check_at = Some(self.next_check_at);
        u.error = self.error.clone();
    }
}

/// The updater's state in this process.
#[derive(Debug, Default)]
struct State {
    /// `updater.json` as last written (or read at start): every write
    /// starts from it, and only a write that succeeded replaces it.
    durable: Updater,
    /// The last check's times when they couldn't be written: shown and
    /// scheduled from (so a `<data>/bin` that can't be written doesn't
    /// make the schedule check back to back), never written as they are.
    unsaved_check: Option<CheckTimes>,
    /// A pause failed to save, or `updater.json` couldn't be read at
    /// start: automatic installs are held in this process until a pause
    /// or resume is saved. Never written.
    held: bool,
    /// `updater.json` was there but couldn't be read at start, so a pause
    /// in it is unknown: nothing but an explicit pause or resume writes
    /// over it (a check's times stay in memory).
    unreadable: bool,
}

/// What readers see of [`State`]: published at the end of each change,
/// so a reader on an async worker never waits on a write's fsync.
#[derive(Debug, Default, Clone)]
struct View {
    updater: Updater,
    check_unsaved: bool,
    held: bool,
    unreadable: bool,
}

impl State {
    fn view(&self) -> View {
        View {
            updater: self.effective(),
            check_unsaved: self.unsaved_check.is_some(),
            held: self.held,
            unreadable: self.unreadable,
        }
    }

    /// The state in effect: the durable one with any unsaved check.
    fn effective(&self) -> Updater {
        let mut u = self.durable.clone();
        if let Some(c) = &self.unsaved_check {
            c.apply(&mut u);
        }
        u
    }
}

/// Fails a step of a durable write (tests); passes every step otherwise.
type Fault = Box<dyn Fn(Step) -> io::Result<()> + Send + Sync>;

/// What tests replace: the clock, the keys and the durable writes' faults.
struct Seams {
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    keys: Box<dyn Fn() -> Result<lodestar_update::Embedded, lodestar_update::Error> + Send + Sync>,
    fault: Fault,
}

/// Why a durable write didn't happen.
#[derive(Debug)]
enum SaveError {
    /// No `<data>/bin`: nothing can be written.
    NoBin,
    /// A step failed; logged by [`unsaved`].
    Io,
    /// `updater.json` couldn't be read at start (see [`State::unreadable`]).
    Unreadable,
}

/// What the engine knows about the running process.
#[derive(Debug, Clone)]
pub struct Context {
    pub data_dir: PathBuf,
    pub running: Version,
    /// The supervisor's (the image's) version.
    pub image: Version,
    /// Under the supervisor named in the hint.
    pub supervised: bool,
    /// The hint's notices (`bad_overlay`, `revoked_key`, …).
    pub notices: Vec<String>,
    /// `CARGO_PKG_REPOSITORY`.
    pub repository: String,
    /// The store, whose size a release that migrates needs free again
    /// (its snapshot).
    pub db: PathBuf,
    /// A development build: it has no release version to update from.
    pub dev: bool,
}

/// Verifies a staged version as the supervisor will.
type Verify = Box<dyn Fn(&Bin, &Version) -> bool + Send + Sync>;

/// What an install needs from outside the engine.
pub struct Installer {
    /// Where downloads come from.
    pub source: Arc<dyn download::Source>,
    /// What keeps an install waiting (`rebuild_running`), if anything.
    pub busy: Box<dyn Fn() -> Option<&'static str> + Send + Sync>,
    /// Asks for the switch to a staged version: the intent, then the
    /// graceful shutdown.
    pub switch: Box<dyn Fn(&Version) + Send + Sync>,
    /// This build's target; `None` where nothing is shipped for it.
    pub target: Option<lodestar_update::Target>,
    /// Verifies a staged version as the supervisor will before running it.
    pub verify: Verify,
    /// A restart (or shutdown) has been asked for: nothing starts now.
    pub restarting: Box<dyn Fn() -> bool + Send + Sync>,
    /// Asks for the rollback to the previous version (the torrents it
    /// loses, when counted): the intent, then the graceful shutdown.
    pub rollback: Box<dyn Fn(Option<u64>) + Send + Sync>,
}

impl Installer {
    /// The real one: GitHub through the guarded client, and the switch
    /// through `request.json` and `EXIT_SWITCH`.
    pub fn new(
        source: Arc<dyn download::Source>,
        busy: Box<dyn Fn() -> Option<&'static str> + Send + Sync>,
    ) -> Installer {
        Installer {
            source,
            busy,
            switch: Box::new(|v| {
                super::control::set_intent(
                    super::control::Action::Switch,
                    Some(v.to_string()),
                    None,
                );
                crate::system::Restart::global().request();
            }),
            target: super::testing::target()
                .or_else(|| lodestar_update::Target::from_triple(env!("LODESTAR_BUILD_TARGET"))),
            verify: Box::new(|bin, v| super::overlay::verify(bin, v).is_ok()),
            restarting: Box::new(|| crate::system::Restart::global().was_requested()),
            rollback: Box::new(super::control::request_rollback),
        }
    }
}

/// The engine: cheap to clone.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

struct Inner {
    ctx: Context,
    bin: Option<Bin>,
    settings: watch::Receiver<Updates>,
    publish: Publish,
    fetch: Arc<dyn Fetch>,
    zone: (jiff::tz::TimeZone, String),
    found: Mutex<Found>,
    /// The updater's state; held through every write of it, so writes
    /// land in the order they were made and never collide on the
    /// temporary file. Only writers take it; readers use `view`.
    updater: Mutex<State>,
    /// `updater`'s last published [`View`], replaced while `updater` is
    /// held and never held across I/O.
    view: Mutex<View>,
    fault: Fault,
    /// One check or install at a time: an install holds it throughout.
    checking: Arc<tokio::sync::Mutex<()>>,
    last_manual: Mutex<Option<Instant>>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    installer: Installer,
    /// The version an install under way is for; kept after a switch was
    /// asked for, until the process exits.
    installing: Mutex<Option<Version>>,
    /// The one-check lock, kept for the rest of the process once a switch
    /// was asked for: nothing checks, stages or installs again before the
    /// restart (a second stage would replace the folder the switch names).
    switching: Mutex<Option<OneAtATime>>,
    /// The version the last install was for, failed or switching: the
    /// schedule doesn't try it again until the next check.
    failed: Mutex<Option<Version>>,
    last_progress: Mutex<Option<Instant>>,
    /// Wakes the schedule (a version queued for the window).
    wake: tokio::sync::Notify,
    /// The keys checks verify with (the embedded ones, or tests').
    keys: Box<dyn Fn() -> Result<lodestar_update::Embedded, lodestar_update::Error> + Send + Sync>,
}

/// The one-check-or-install lock, held.
type OneAtATime = tokio::sync::OwnedMutexGuard<()>;

/// Where `updates` events go: the event hub, or a test's list.
pub type Publish = Arc<dyn Fn(UpdatesEvent) + Send + Sync>;

/// What the last check found.
#[derive(Default)]
struct Found {
    state: Option<CheckState>,
    checked: Option<Checked>,
    /// The envelope `checked` came from, verified again before an
    /// install (against the trust state and the clock then).
    envelope: Option<Arc<Vec<u8>>>,
    notices: Vec<String>,
    trust_damaged: bool,
}

impl Engine {
    pub fn new(
        ctx: Context,
        settings: watch::Receiver<Updates>,
        hub: Arc<EventHub>,
        fetch: Arc<dyn Fetch>,
        installer: Installer,
    ) -> Engine {
        let (tz, name, _) = super::window::local_zone();
        let publish: Publish = Arc::new(move |e| {
            hub.publish(&Event::Updates(e));
        });
        Engine::with(
            ctx,
            settings,
            publish,
            fetch,
            (tz, name),
            Seams {
                clock: Box::new(unix_now),
                keys: Box::new(super::trust_root),
                fault: Box::new(|_| Ok(())),
            },
            installer,
        )
    }

    fn with(
        ctx: Context,
        settings: watch::Receiver<Updates>,
        publish: Publish,
        fetch: Arc<dyn Fetch>,
        zone: (jiff::tz::TimeZone, String),
        Seams { clock, keys, fault }: Seams,
        installer: Installer,
    ) -> Engine {
        let bin = Bin::open(&ctx.data_dir).ok();
        // An `updater.json` that's there but can't be read may hold a
        // pause: fail closed (hold, and never write over it unasked)
        // rather than start unpaused and let the next check erase it.
        let (durable, unreadable) = match bin.as_ref().map(|b| b.read::<Updater>(UPDATER_FILE)) {
            Some(Ok(u)) => (u.unwrap_or_default(), false),
            Some(Err(e)) => {
                log_event!(UPDATE_STATE_UNREADABLE, error = lodestar_log::Error::io(&e));
                (Updater::default(), true)
            }
            None => (Updater::default(), false),
        };
        let state = State {
            durable,
            unsaved_check: None,
            held: unreadable,
            unreadable,
        };
        Engine {
            inner: Arc::new(Inner {
                ctx,
                bin,
                settings,
                publish,
                fetch,
                zone,
                found: Mutex::new(Found::default()),
                view: Mutex::new(state.view()),
                updater: Mutex::new(state),
                fault,
                checking: Arc::new(tokio::sync::Mutex::new(())),
                last_manual: Mutex::new(None),
                clock,
                installer,
                installing: Mutex::new(None),
                switching: Mutex::new(None),
                failed: Mutex::new(None),
                last_progress: Mutex::new(None),
                wake: tokio::sync::Notify::new(),
                keys,
            }),
        }
    }

    /// The scheduled checks and installs, until `stop` is set.
    pub fn spawn(&self, mut stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        let engine = self.clone();
        let mut settings = self.inner.settings.clone();
        tokio::spawn(async move {
            engine.announce_start().await;
            loop {
                let wait = match (engine.until_scheduled(), engine.until_install()) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                tokio::select! {
                    _ = stop.changed() => return,
                    r = settings.changed() => {
                        if r.is_err() { return; }
                        continue;
                    }
                    () = engine.inner.wake.notified() => continue,
                    () = sleep(wait.map(|w| w.min(MAX_SLEEP))) => {}
                }
                if engine.switching() {
                    // The switch keeps the lock until the process exits.
                    let _ = stop.changed().await;
                    return;
                }
                if engine.due() {
                    // Still due once the lock is ours: a manual check may
                    // have run while this one waited for it.
                    let Some(one) = engine.one_at_a_time(&mut stop).await else {
                        return;
                    };
                    if engine.due() {
                        let _ = engine.run_check_holding(one).await;
                    }
                }
                if engine.until_install().is_some_and(|d| d.is_zero()) {
                    let Some(one) = engine.one_at_a_time(&mut stop).await else {
                        return;
                    };
                    // Still due once the lock is ours (an Install now may
                    // have run meanwhile).
                    if let Some((v, true)) = engine.next_install() {
                        let _ = engine.run_install(one, v).await;
                    }
                }
            }
        })
    }

    /// The one-check lock, or `None` once `stop` is set first (a switch
    /// may hold the lock until the process exits).
    async fn one_at_a_time(&self, stop: &mut watch::Receiver<bool>) -> Option<OneAtATime> {
        tokio::select! {
            one = self.inner.checking.clone().lock_owned() => Some(one),
            _ = stop.changed() => None,
        }
    }

    /// A switch was asked for: this process is on its way out.
    fn switching(&self) -> bool {
        lock(&self.inner.switching).is_some() || (self.inner.installer.restarting)()
    }

    /// The install the schedule makes next and whether it's due now.
    fn next_install(&self) -> Option<(Version, bool)> {
        self.install_at()
            .map(|(v, at)| (v, at <= (self.inner.clock)()))
    }

    /// A version queued for the window, else the policy's next automatic
    /// install; never one whose install failed since the last check.
    fn install_at(&self) -> Option<(Version, i64)> {
        let settings = self.inner.settings.borrow().clone();
        let now = (self.inner.clock)();
        let failed = lock(&self.inner.failed).clone();
        if let Some(q) = self
            .updater()
            .queued
            .as_deref()
            .and_then(layout::parse_version)
        {
            if failed.as_ref() == Some(&q) {
                return None;
            }
            let at = Window::parse(&settings.window).ok().map_or(now, |w| {
                match w.when(now, &self.inner.zone.0) {
                    When::Open { .. } => now,
                    When::Opens(at) => at,
                }
            });
            return Some((q, at));
        }
        let (v, at) = self.plan(&settings, now).next?;
        (failed.as_ref() != Some(&v)).then_some((v, at))
    }

    /// How long until the next install; `None` when there's none.
    fn until_install(&self) -> Option<Duration> {
        let (_, at) = self.install_at()?;
        let now = (self.inner.clock)();
        Some(Duration::from_secs(at.saturating_sub(now).max(0) as u64))
    }

    /// At start: `checking_health` while this version is in probation (then
    /// `done` once its self-test passes), and `rolled_back` once for a
    /// rollback the history records that wasn't announced yet.
    async fn announce_start(&self) {
        let running = self.inner.ctx.running.to_string();
        let probation = self
            .inner
            .bin
            .as_ref()
            .and_then(|b| pending::read(b).ok().flatten())
            .is_some_and(|p| p.version == running && p.kind == pending::Kind::Install);
        if probation {
            self.publish(UpdatesState::CheckingHealth, Some(running.clone()), None);
            let e = self.clone();
            tokio::spawn(async move {
                super::probation::passed().await;
                e.publish(UpdatesState::Done, Some(running), None);
            });
        }
        let h: history::History = self
            .inner
            .bin
            .as_ref()
            .and_then(|b| b.read(history::FILE).ok().flatten())
            .unwrap_or_default();
        if let Some(last) = h.entries.last()
            && last.result == history::Outcome::RolledBack
            && self
                .updater()
                .rolled_back_seen
                .is_none_or(|seen| last.at > seen)
        {
            self.publish(UpdatesState::RolledBack, Some(last.version.clone()), None);
            // Best effort: unsaved, it's announced again at the next start.
            let (e, at) = (self.clone(), last.at);
            let _ = blocking(move || e.save_updater(|u| u.rolled_back_seen = Some(at))).await;
        }
    }

    fn scheduled_on(&self) -> bool {
        self.inner.settings.borrow().mode != Mode::Off
    }

    /// How long until the next scheduled check; `None` when there are
    /// none (updates off).
    fn until_scheduled(&self) -> Option<Duration> {
        if !self.scheduled_on() {
            return None;
        }
        let now = (self.inner.clock)();
        let next = self.updater().next_check_at.unwrap_or(now);
        // Further away than one interval can be (a clock that ran ahead
        // and was corrected, a board that booted at 1970): due now, or
        // the schedule would sleep until then.
        if next > now.saturating_add(EVERY.as_secs() as i64 + 3600) {
            return Some(Duration::ZERO);
        }
        Some(Duration::from_secs(next.saturating_sub(now).max(0) as u64))
    }

    /// A scheduled check is due now.
    fn due(&self) -> bool {
        self.until_scheduled().is_some_and(|d| d.is_zero())
    }

    /// The updater state in effect (any unsaved check included).
    fn updater(&self) -> Updater {
        lock(&self.inner.view).updater.clone()
    }

    /// Changes the updater's state under its lock, then publishes the
    /// readers' view before the lock is released.
    fn change<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let mut state = lock(&self.inner.updater);
        let r = f(&mut state);
        *lock(&self.inner.view) = state.view();
        r
    }

    /// The one way `updater.json` changes: `f` applied to the durable
    /// state, written durably, and only then made the durable state.
    /// The caller holds `state`'s lock throughout, so an older change
    /// can't land after a newer one.
    fn write_updater(
        &self,
        state: &mut State,
        f: impl FnOnce(&mut Updater),
    ) -> Result<(), SaveError> {
        let bin = self.inner.bin.as_ref().ok_or(SaveError::NoBin)?;
        let mut next = state.durable.clone();
        next.format = 1;
        f(&mut next);
        bin.write_acknowledged(UPDATER_FILE, &next, &*self.inner.fault)
            .map_err(|e| unsaved(UPDATER_FILE, e))?;
        state.durable = next;
        Ok(())
    }

    /// Saves a check's times; when they can't be written they're kept in
    /// memory only, and the durable state (a pause) is untouched.
    fn save_check(&self, times: CheckTimes) {
        self.change(|state| {
            if state.unreadable {
                state.unsaved_check = Some(times);
                return;
            }
            state.unsaved_check = match self.write_updater(state, |u| times.apply(u)) {
                Ok(()) => None,
                Err(_) => Some(times),
            };
        });
    }

    /// An install's change to `updater.json` (the queue, the rollback
    /// seen) through [`Engine::write_updater`]. Refused while an
    /// unreadable file may hold a pause: only a pause or resume replaces
    /// it.
    fn save_updater(&self, f: impl FnOnce(&mut Updater)) -> Result<(), SaveError> {
        self.change(|state| {
            if state.unreadable {
                return Err(SaveError::Unreadable);
            }
            self.write_updater(state, f)
        })
    }

    fn publish(&self, state: UpdatesState, version: Option<String>, error: Option<String>) {
        (self.inner.publish)(UpdatesEvent {
            state,
            version,
            bytes_done: None,
            bytes_total: None,
            error,
        });
    }

    /// One check: fetch, verify, advance `trust.json`, keep the result.
    #[cfg(test)]
    async fn run_check(&self) -> Result<(), &'static str> {
        let one = self.inner.checking.clone().lock_owned().await;
        self.run_check_holding(one).await
    }

    /// [`Engine::run_check`] with the one-at-a-time lock already held.
    async fn run_check_holding(&self, _one: OneAtATime) -> Result<(), &'static str> {
        let settings = self.inner.settings.borrow().clone();
        let channel = settings.channel.update();
        self.set_state(CheckState::Checking);
        self.publish(UpdatesState::Checking, None, None);
        let result = self.check_once(channel).await;
        // A new check gives an automatic install that failed another try.
        *lock(&self.inner.failed) = None;
        let now = (self.inner.clock)();
        let next = now + EVERY.as_secs() as i64 + jitter();
        let times = CheckTimes {
            checked_at: now,
            next_check_at: next,
            error: result.err().map(str::to_string),
        };
        // The write fsyncs: off the async workers.
        let e = self.clone();
        blocking(move || e.save_check(times)).await;
        match &result {
            Ok(()) => {
                self.set_state(CheckState::Ok);
                let newest = self
                    .plan(&settings, now)
                    .releases
                    .first()
                    .map(|a| a.version.to_string());
                // Every check ends with an event, so the screen never stays
                // on "checking".
                match newest {
                    Some(v) => {
                        log_event!(UPDATE_AVAILABLE);
                        self.publish(UpdatesState::Available, Some(v), None);
                    }
                    None => self.publish(UpdatesState::Checked, None, None),
                }
            }
            Err(code) => {
                self.set_state(CheckState::Failed);
                log_event!(UPDATE_CHECK_FAILED, error = *code);
                self.publish(UpdatesState::Failed, None, Some(code.to_string()));
            }
        }
        result
    }

    fn set_state(&self, s: CheckState) {
        lock(&self.inner.found).state = Some(s);
    }

    async fn check_once(&self, channel: lodestar_update::Channel) -> Result<(), &'static str> {
        // Without `<data>/bin` there's no trust state to check against:
        // nothing is fetched.
        let Some(bin) = self.inner.bin.clone() else {
            return Err("bin_unavailable");
        };
        let url = check::source(&self.inner.ctx.repository, channel).map_err(CheckError::code)?;
        let body = check::fetch(&*self.inner.fetch, url)
            .await
            .map_err(CheckError::code)?;
        let Some(body) = body else {
            // Nothing published for this channel yet.
            let mut f = lock(&self.inner.found);
            f.checked = None;
            f.envelope = None;
            f.notices.clear();
            f.trust_damaged = false;
            return Ok(());
        };
        let body = Arc::new(body);
        let (checked, damaged) = self.verify(&bin, channel, body.clone(), true).await?;
        let mut f = lock(&self.inner.found);
        f.trust_damaged = damaged;
        match checked {
            Ok(c) => {
                f.notices = c.notices.iter().map(|n| n.code().to_string()).collect();
                f.checked = Some(c);
                f.envelope = Some(body);
                Ok(())
            }
            Err(e) => {
                f.notices = e
                    .notice()
                    .map(|n| n.code().to_string())
                    .into_iter()
                    .collect();
                Err(e.code())
            }
        }
    }

    /// Verifies `body` against `trust.json` as it is now, raised to the
    /// floor; `save` writes the next trust state (forward-only). Also the
    /// damaged flag.
    async fn verify(
        &self,
        bin: &Bin,
        channel: lodestar_update::Channel,
        body: Arc<Vec<u8>>,
        save: bool,
    ) -> Result<(Result<Checked, lodestar_update::Error>, bool), &'static str> {
        let embedded = (self.inner.keys)().map_err(lodestar_update::Error::code)?;
        let now = (self.inner.clock)();
        let bin = bin.clone();
        tokio::task::spawn_blocking(move || {
            // The floor (§12.2): every installed overlay's statements and
            // marks count whether or not trust.json exists. A mark comes
            // only from an overlay verified against the embedded keys
            // (signature, binary hash and size), so a planted directory
            // can't raise a mark and freeze updates.
            // An overlay whose signer was revoked since, or whose manifest
            // is dated more than a day ahead, gives no mark either.
            let ahead = (now.max(0) as u64).saturating_add(lodestar_update::MAX_AHEAD);
            let marks: Vec<_> = bin
                .overlays_skipping(|e| {
                    log_event!(
                        UPDATE_OVERLAY_UNREADABLE,
                        error = lodestar_log::Error::io(e)
                    );
                })
                .unwrap_or_default()
                .iter()
                .filter_map(|v| super::overlay::verify(&bin, v).ok())
                .filter(|v| !v.overlay.revoked_later && v.overlay.mark.timestamp <= ahead)
                .map(|v| v.overlay.mark)
                .collect();
            let loaded = trust_file::load(bin.dir(), &embedded, &marks);
            let damaged = loaded.damaged();
            let checked = lodestar_update::check(
                &body,
                channel,
                now.max(0) as u64,
                &embedded,
                loaded.state(),
            );
            if save && let Ok(c) = &checked {
                let _ = trust_file::save(bin.dir(), &loaded, c.next.clone());
            }
            (checked, damaged)
        })
        .await
        .map_err(|_| "internal")
    }

    /// The skipped versions; `None` when `skipped` can't be read, which
    /// blocks automatic installs rather than forgetting a skip.
    fn skipped(&self) -> Option<Vec<String>> {
        #[derive(Deserialize)]
        struct Skip {
            version: String,
        }
        let Some(bin) = &self.inner.bin else {
            return Some(vec![]);
        };
        let list = bin.read::<Vec<Skip>>("skipped").ok()?.unwrap_or_default();
        Some(list.into_iter().map(|s| s.version).collect())
    }

    /// Why this installation can't install itself; `None` when it can.
    fn incapable(
        &self,
        settings: &Updates,
        trust_damaged: bool,
        skipped_ok: bool,
    ) -> Option<Reason> {
        self.incapable_reason(settings, trust_damaged, skipped_ok)
            .err()
    }

    fn incapable_reason(
        &self,
        settings: &Updates,
        trust_damaged: bool,
        skipped_ok: bool,
    ) -> Result<(), Reason> {
        let ctx = &self.inner.ctx;
        if !ctx.supervised {
            return Err(Reason::NotSupervised);
        }
        // Named before the rest: a broken file is a fault to fix, and it
        // blocks automatic installs rather than forgetting a skip.
        if !skipped_ok {
            return Err(Reason::SkippedUnreadable);
        }
        if ctx.dev {
            return Err(Reason::DevBuild);
        }
        if std::env::var("LODESTAR_DEPLOYMENT").is_ok_and(|d| d.trim() == "package") {
            return Err(Reason::PackageManaged);
        }
        if settings.mode == Mode::Image {
            return Err(Reason::ImageMode);
        }
        let Some(bin) = &self.inner.bin else {
            return Err(Reason::NotWritable);
        };
        match noexec(bin.dir()) {
            Some(true) => return Err(Reason::Noexec),
            // Not known isn't "not noexec".
            None => return Err(Reason::NoexecUnknown),
            Some(false) => {}
        }
        if rustix::fs::access(bin.dir(), rustix::fs::Access::WRITE_OK).is_err() {
            return Err(Reason::NotWritable);
        }
        if trust_damaged {
            return Err(Reason::TrustDamaged);
        }
        let keys = (self.inner.keys)().ok();
        let role = settings.channel.update().role();
        let has_key = keys.is_some_and(|k| match role {
            lodestar_update::Role::Release => k.release.is_some(),
            lodestar_update::Role::Nightly => k.nightly.is_some(),
            _ => false,
        });
        if !has_key {
            return Err(Reason::ChannelUnavailable);
        }
        Ok(())
    }

    /// [`Engine::plan_with`] over the updater state now.
    fn plan(&self, settings: &Updates, now: i64) -> policy::Plan {
        let view = lock(&self.inner.view).clone();
        self.plan_with(settings, now, (view.updater.paused_until, view.held))
    }

    /// The policy's plan, given the durable pause and the hold (one
    /// snapshot of them, so a response never mixes two).
    fn plan_with(&self, settings: &Updates, now: i64, pause: (Option<i64>, bool)) -> policy::Plan {
        self.with_inputs(settings, now, pause, policy::plan)
            .unwrap_or_default()
    }

    /// `f` over the policy's inputs as they are now, given the durable
    /// pause and the hold; `None` before a check found a manifest.
    fn with_inputs<T>(
        &self,
        settings: &Updates,
        now: i64,
        (paused_until, held_in_process): (Option<i64>, bool),
        f: impl FnOnce(&policy::Inputs<'_>) -> T,
    ) -> Option<T> {
        let found = lock(&self.inner.found);
        let c = found.checked.as_ref()?;
        let window = Window::parse(&settings.window).ok();
        let tz = self.inner.zone.0.clone();
        let window_from = move |t: i64| match window.map(|w| w.when(t, &tz)) {
            Some(When::Open { .. }) | None => t,
            Some(When::Opens(at)) => at,
        };
        let skipped = self.skipped();
        let incapable = self.incapable(settings, found.trust_damaged, skipped.is_some());
        let skipped = skipped.unwrap_or_default();
        Some(f(&policy::Inputs {
            settings,
            releases: &c.manifest.releases,
            current: current(c, now),
            running: &self.inner.ctx.running,
            skipped: &skipped,
            paused_until,
            held_in_process,
            now,
            window_from: &window_from,
            incapable,
        }))
    }

    /// Everything `GET /updates` reports.
    pub fn status(&self) -> UpdatesStatus {
        let settings = self.inner.settings.borrow().clone();
        let now = (self.inner.clock)();
        let ctx = &self.inner.ctx;
        let View {
            updater,
            check_unsaved,
            held: held_in_process,
            unreadable,
        } = lock(&self.inner.view).clone();
        let plan = self.plan_with(&settings, now, (updater.paused_until, held_in_process));
        let found = lock(&self.inner.found);
        let reason = self.incapable(&settings, found.trust_damaged, self.skipped().is_some());
        let running_release = found.checked.as_ref().and_then(|c| {
            c.manifest
                .releases
                .iter()
                .find(|r| r.version == ctx.running)
        });
        let mut notices: Vec<String> = ctx.notices.clone();
        notices.extend(found.notices.iter().cloned());
        if found.trust_damaged {
            notices.push(Notice::TrustDamaged.code().to_string());
        }
        if found.checked.as_ref().is_some_and(|c| !current(c, now)) {
            notices.push("manifest_lapsed".into());
        }
        // Without `<data>/bin` nothing can be saved, which `bin_unavailable`
        // already says.
        if check_unsaved && self.inner.bin.is_some() {
            notices.push("check_not_saved".into());
        }
        if unreadable {
            notices.push("updater_unreadable".into());
        }
        let history = self.history();
        if history.last().is_some_and(|h| h.result == "rolled_back") {
            notices.push("rolled_back".into());
        }
        notices.sort();
        notices.dedup();
        let releases = found
            .checked
            .as_ref()
            .map(|c| release_infos(&plan, &c.manifest.releases))
            .unwrap_or_default();
        let window = Window::parse(&settings.window).ok();
        let next_window_at = window.map(|w| match w.when(now, &self.inner.zone.0) {
            When::Open { .. } => now,
            When::Opens(at) => at,
        });
        UpdatesStatus {
            unavailable: None,
            status: CheckStatus {
                state: found.state.unwrap_or(CheckState::Idle),
                checked_at: updater.checked_at,
                next_check_at: updater.next_check_at.filter(|_| settings.mode != Mode::Off),
                error: updater.error.clone(),
            },
            running: Running {
                version: ctx.running.to_string(),
                channel: channel_name(&settings).into(),
                image_version: (ctx.image != ctx.running).then(|| ctx.image.to_string()),
                overlay: ctx.image != ctx.running,
                revoked_key: ctx.notices.iter().any(|n| n == "revoked_key"),
                withdrawn: running_release.is_some_and(|r| r.withdrawn),
            },
            capability: Capability {
                can_install: reason.is_none(),
                reason: reason.map(|r| r.code().to_string()),
            },
            policy: Policy {
                mode: mode_name(settings.mode).into(),
                level: level_name(&settings).into(),
                security_fixes: settings.security_effective(),
                wait_days: settings.wait_days,
                window: settings.window.clone(),
                window_zone: self.inner.zone.1.clone(),
                next_window_at,
                paused_until: updater.paused_until.filter(|&p| p > now),
                held_in_process,
                allow_reindex: settings.allow_reindex,
            },
            releases,
            pending: self.pending(),
            installing: self.installing(),
            rollback: self.rollback(),
            notices,
            history,
        }
    }

    fn history(&self) -> Vec<HistoryEntry> {
        let h: history::History = self
            .inner
            .bin
            .as_ref()
            .and_then(|b| b.read(history::FILE).ok().flatten())
            .unwrap_or_default();
        h.entries
            .into_iter()
            .map(|e| HistoryEntry {
                version: e.version,
                from: e.from,
                level: e.level,
                at: e.at as i64,
                result: outcome_name(e.result).into(),
                reason: e.reason,
                lost_torrents: e.lost_torrents,
            })
            .collect()
    }

    fn pending(&self) -> Option<PendingInstall> {
        let p = pending::read(self.inner.bin.as_ref()?).ok().flatten()?;
        Some(PendingInstall {
            version: p.version,
            kind: match p.kind {
                pending::Kind::Install => "install",
                pending::Kind::Restored => "restored",
            }
            .into(),
            attempts: p.attempts,
        })
    }

    /// Whether Roll back is offered now, why not, and where it goes.
    fn rollback(&self) -> RollbackInfo {
        let active = self.active();
        let reason = match self.rollback_target() {
            // A check or an install holds the lock: it waits for that.
            Ok(_) if self.inner.checking.try_lock().is_err() => Some("busy"),
            Ok(_) => None,
            Err(r) => Some(r),
        };
        RollbackInfo {
            available: reason.is_none(),
            reason: reason.map(str::to_string),
            to_version: active.map(|a| a.previous),
            lost_torrents: super::probation::current().and_then(|c| c.lost_torrents()),
        }
    }

    /// `active`, when it names the running version (the one Roll back
    /// rolls back).
    fn active(&self) -> Option<pending::Active> {
        self.inner
            .bin
            .as_ref()
            .and_then(|b| b.read("active").ok().flatten())
            .filter(|a: &pending::Active| a.version == self.inner.ctx.running.to_string())
    }

    /// What Roll back would roll back (`active`), or why it can't now:
    /// the checks before the supervisor's own (step 2 of the journal,
    /// which verifies the snapshot and can still refuse).
    fn rollback_target(&self) -> Result<pending::Active, &'static str> {
        let ctx = &self.inner.ctx;
        if !ctx.supervised {
            return Err("not_supervised");
        }
        if self.inner.settings.borrow().mode == Mode::Image {
            return Err("image_mode");
        }
        if self.switching() {
            return Err("busy");
        }
        let bin = self.inner.bin.as_ref().ok_or("no_previous_version")?;
        let active = self.active().ok_or("no_previous_version")?;
        let jctx = journal::Ctx {
            bin: bin.clone(),
            db: ctx.db.clone(),
            index_root: ctx.data_dir.join("index"),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            image_version: ctx.image.to_string(),
        };
        if !journal::kept(&jctx, &active.version) {
            return Err("snapshot_missing");
        }
        Ok(active)
    }

    /// `POST /updates/rollback`: takes the one-check lock for the rest of
    /// the process (nothing checks or installs before the restart), then
    /// asks for the rollback; the supervisor runs the journal after the
    /// graceful shutdown. Refused while a check or an install holds the
    /// lock, or a restart was asked for (`busy`).
    pub async fn rollback_request(&self) -> Result<UpdatesStatus, UpdatesError> {
        let e = self.clone();
        blocking(move || e.rollback_target())
            .await
            .map_err(UpdatesError::RollbackUnavailable)?;
        let Ok(one) = self.inner.checking.clone().try_lock_owned() else {
            return Err(UpdatesError::RollbackUnavailable("busy"));
        };
        // Again with the lock held: an install may have asked for its
        // switch meanwhile.
        let e = self.clone();
        let active = blocking(move || e.rollback_target())
            .await
            .map_err(UpdatesError::RollbackUnavailable)?;
        *lock(&self.inner.switching) = Some(one);
        self.publish(UpdatesState::Restarting, Some(active.previous), None);
        (self.inner.installer.rollback)(
            super::probation::current().and_then(|c| c.lost_torrents()),
        );
        let e = self.clone();
        Ok(blocking(move || e.status()).await)
    }

    /// A manual check, at most once a minute and one at a time.
    pub async fn check_now(&self) -> Result<UpdatesStatus, UpdatesError> {
        let one = {
            let mut last = lock(&self.inner.last_manual);
            if let Some(at) = *last {
                let since = at.elapsed();
                if since < MANUAL_EVERY {
                    return Err(UpdatesError::RateLimited {
                        retry_after: (MANUAL_EVERY - since).as_secs().max(1),
                    });
                }
            }
            // Held through the check, so a scheduled one can't slip in
            // between and make this a second check right after it.
            let Ok(one) = self.inner.checking.clone().try_lock_owned() else {
                return Err(UpdatesError::RateLimited { retry_after: 5 });
            };
            *last = Some(Instant::now());
            one
        };
        let _ = self.run_check_holding(one).await;
        let e = self.clone();
        Ok(blocking(move || e.status()).await)
    }

    /// `POST /updates/install`: checks it may install, then starts it (or
    /// queues it for the window) and answers at once.
    pub async fn install_request(
        &self,
        version: &str,
        when: InstallWhen,
        confirm_manual: bool,
    ) -> Result<UpdatesStatus, UpdatesError> {
        let v = layout::parse_version(version).ok_or(UpdatesError::UnknownVersion)?;
        if self.switching() {
            return Err(UpdatesError::InstallUnavailable("busy"));
        }
        self.may_install(&v, confirm_manual)?;
        match when {
            InstallWhen::Window => {
                let (e, queued) = (self.clone(), v.to_string());
                blocking(move || e.save_updater(|u| u.queued = Some(queued)))
                    .await
                    .map_err(|e| match e {
                        SaveError::NoBin => UpdatesError::BinUnavailable,
                        SaveError::Io => UpdatesError::NotSaved,
                        SaveError::Unreadable => {
                            UpdatesError::InstallUnavailable("updater_unreadable")
                        }
                    })?;
                self.publish(UpdatesState::WaitingWindow, Some(v.to_string()), None);
                self.inner.wake.notify_one();
            }
            InstallWhen::Now => {
                let Ok(one) = self.inner.checking.clone().try_lock_owned() else {
                    return Err(UpdatesError::InstallUnavailable("busy"));
                };
                // Shown in the answer below, before the task starts.
                *lock(&self.inner.installing) = Some(v.clone());
                let e = self.clone();
                tokio::spawn(async move {
                    let _ = e.run_install(one, v).await;
                });
            }
        }
        let e = self.clone();
        Ok(blocking(move || e.status()).await)
    }

    /// Install now's rules (the policy's), and the confirmation a release
    /// with manual steps needs.
    fn may_install(&self, v: &Version, confirm_manual: bool) -> Result<(), UpdatesError> {
        let settings = self.inner.settings.borrow().clone();
        let now = (self.inner.clock)();
        let view = lock(&self.inner.view).clone();
        let manual = self
            .with_inputs(
                &settings,
                now,
                (view.updater.paused_until, view.held),
                |i| {
                    policy::install_now(i, v)?;
                    Ok(i.releases.iter().any(|r| r.version == *v && r.manual_steps))
                },
            )
            .unwrap_or(Err(Refusal::UnknownVersion))
            .map_err(|r| match r {
                Refusal::UnknownVersion => UpdatesError::UnknownVersion,
                Refusal::Skipped => UpdatesError::VersionSkipped,
                Refusal::NotCandidate => UpdatesError::InstallUnavailable("not_candidate"),
                Refusal::Incapable(reason) => UpdatesError::InstallUnavailable(reason.code()),
                Refusal::NotCurrent => UpdatesError::InstallUnavailable("not_current"),
                Refusal::UpgradePath => UpdatesError::InstallUnavailable("upgrade_path"),
            })?;
        if manual && !confirm_manual {
            return Err(UpdatesError::ManualConfirmationRequired);
        }
        Ok(())
    }

    /// One install, holding the one-check lock: its events, and on failure
    /// the log line and the `failed` event. A queued version is taken off
    /// the queue either way.
    async fn run_install(&self, one: OneAtATime, v: Version) -> Result<(), &'static str> {
        *lock(&self.inner.installing) = Some(v.clone());
        let result = self.install_steps(&v).await;
        if result.is_ok() {
            // The switch was asked for: keep the lock and `installing`
            // until the process exits.
            *lock(&self.inner.switching) = Some(one);
        } else {
            *lock(&self.inner.installing) = None;
            drop(one);
        }
        // Not again until the next check, whether it failed or is
        // switching (the restart may take a moment).
        *lock(&self.inner.failed) = Some(v.clone());
        let queued = self.updater().queued.as_deref() == Some(v.to_string().as_str());
        if queued {
            // Best effort: one left queued is held back by `failed`, or
            // refused as `not_candidate` once this version runs.
            let e = self.clone();
            let _ = blocking(move || e.save_updater(|u| u.queued = None)).await;
        }
        if let Err(code) = result {
            log_event!(UPDATE_INSTALL_FAILED, error = code);
            self.publish(
                UpdatesState::Failed,
                Some(v.to_string()),
                Some(code.to_string()),
            );
        }
        result
    }

    async fn install_steps(&self, v: &Version) -> Result<(), &'static str> {
        let bin = self.inner.bin.clone().ok_or("bin_unavailable")?;
        // Policy again (a queued version may have been skipped since); the
        // manual steps were confirmed when it was asked for.
        self.may_install(v, true).map_err(|e| match e {
            UpdatesError::VersionSkipped => "version_skipped",
            UpdatesError::InstallUnavailable(r) => r,
            _ => "unknown_version",
        })?;
        if let Some(busy) = (self.inner.installer.busy)() {
            return Err(busy);
        }
        let (checked, release) = self.reverify(&bin, v).await?;
        let target = self.inner.installer.target.ok_or("no_artifact")?;
        let pin =
            lodestar_update::install(&checked, v, target, lodestar_update::ArtifactKind::Binary)
                .map_err(lodestar_update::Error::code)?;
        self.room_for(&bin, &pin, &release)?;

        *lock(&self.inner.last_progress) = None;
        self.progress(v, 0, pin.size, true);
        let part = download::download(&*self.inner.installer.source, &bin, &pin, &mut |n| {
            self.progress(v, n, pin.size, n == pin.size)
        })
        .await
        .map_err(download::DownloadError::code)?;

        self.publish(UpdatesState::Verifying, Some(v.to_string()), None);
        let (b, version) = (bin.clone(), v.clone());
        let (body, minisig) = (
            checked.manifest.body.clone(),
            checked.manifest.minisig.clone(),
        );
        blocking(move || install::stage(&b, &version, &part, &body, &minisig))
            .await
            .map_err(|_| "stage_failed")?;
        // As the supervisor will verify it before running it.
        let e = self.clone();
        let (b, version) = (bin.clone(), v.clone());
        if !blocking(move || (e.inner.installer.verify)(&b, &version)).await {
            let _ = bin.remove_overlay(v);
            return Err("bad_overlay");
        }
        // §12.2 "At install time": against the trust state and clock now.
        // A refusal removes the staged folder, as a bad overlay's is.
        let again = self.reverify(&bin, v).await.and_then(|(again, _)| {
            lodestar_update::install(&again, v, target, lodestar_update::ArtifactKind::Binary)
                .map(|_| ())
                .map_err(lodestar_update::Error::code)
        });
        if let Err(code) = again {
            let _ = bin.remove_overlay(v);
            return Err(code);
        }

        self.publish(UpdatesState::Installing, Some(v.to_string()), None);
        (self.inner.installer.switch)(v);
        self.publish(UpdatesState::Restarting, Some(v.to_string()), None);
        Ok(())
    }

    /// The last check's envelope verified again now (the trust state, the
    /// clock, the marks); its result must be current and still list `v`.
    async fn reverify(
        &self,
        bin: &Bin,
        v: &Version,
    ) -> Result<(Checked, lodestar_update::Release), &'static str> {
        let envelope = lock(&self.inner.found)
            .envelope
            .clone()
            .ok_or("not_checked")?;
        let channel = self.inner.settings.borrow().channel.update();
        let (checked, damaged) = self.verify(bin, channel, envelope, false).await?;
        if damaged {
            return Err(Notice::TrustDamaged.code());
        }
        let checked = checked.map_err(lodestar_update::Error::code)?;
        if !current(&checked, (self.inner.clock)()) {
            return Err("not_current");
        }
        let release = checked
            .manifest
            .releases
            .iter()
            .find(|r| r.version == *v)
            .cloned()
            .ok_or("unknown_version")?;
        Ok((checked, release))
    }

    /// Room on `<data>/bin`'s disk for the binary, the index rebuild a
    /// release needs and the store's snapshot when it migrates, plus a
    /// margin.
    fn room_for(
        &self,
        bin: &Bin,
        pin: &lodestar_update::Pin,
        release: &lodestar_update::Release,
    ) -> Result<(), &'static str> {
        let snapshot = if release.migrates {
            std::fs::metadata(&self.inner.ctx.db).map_or(0, |m| m.len())
        } else {
            0
        };
        let need = pin
            .size
            .saturating_add(release.reindex_bytes)
            .saturating_add(snapshot)
            .saturating_add(SPACE_MARGIN);
        let free = rustix::fs::statvfs(bin.dir())
            .map(|s| s.f_bavail.saturating_mul(s.f_frsize))
            .map_err(|_| "no_space")?;
        if free < need {
            return Err("no_space");
        }
        Ok(())
    }

    /// `downloading`, at most once a second (and always when `force`).
    fn progress(&self, v: &Version, done: u64, total: u64, force: bool) {
        {
            let mut last = lock(&self.inner.last_progress);
            if !force && last.is_some_and(|t| t.elapsed() < PROGRESS_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        (self.inner.publish)(UpdatesEvent {
            state: UpdatesState::Downloading,
            version: Some(v.to_string()),
            bytes_done: Some(done),
            bytes_total: Some(total),
            error: None,
        });
    }

    async fn install_request_owned(
        &self,
        version: String,
        when: InstallWhen,
        confirm_manual: bool,
    ) -> Result<UpdatesStatus, UpdatesError> {
        self.install_request(&version, when, confirm_manual).await
    }

    /// The install under way, else the one queued for the window.
    fn installing(&self) -> Option<Installing> {
        if let Some(v) = lock(&self.inner.installing).clone() {
            return Some(Installing {
                version: v.to_string(),
                when: InstallWhen::Now,
            });
        }
        self.updater().queued.map(|version| Installing {
            version,
            when: InstallWhen::Window,
        })
    }

    pub fn skip(&self, version: &str) -> Result<UpdatesStatus, UpdatesError> {
        let known = lock(&self.inner.found).checked.as_ref().is_some_and(|c| {
            c.manifest
                .releases
                .iter()
                .any(|r| r.version.to_string() == version)
        });
        if !known {
            return Err(UpdatesError::UnknownVersion);
        }
        let bin = self
            .inner
            .bin
            .as_ref()
            .ok_or(UpdatesError::BinUnavailable)?;
        {
            // Under the updater's lock: one writer of its state at a time.
            let _state = lock(&self.inner.updater);
            // Only a failed write is "not saved"; an unreadable list is
            // damage, as before.
            let mut writing = false;
            pending::skip_with(bin, version, "user", |list| {
                writing = true;
                bin.write_acknowledged(pending::SKIPPED, list, &*self.inner.fault)
            })
            .map_err(|e| {
                if writing {
                    unsaved(pending::SKIPPED, e);
                    UpdatesError::NotSaved
                } else {
                    UpdatesError::Internal(e.to_string())
                }
            })?;
        }
        Ok(self.status())
    }

    /// Pauses automatic installs for `days` (0 resumes), acknowledged
    /// only once it's durable. A pause that can't be saved holds
    /// automatic installs in this process instead; a resume that can't
    /// be saved leaves the pause and any hold as they were.
    pub fn pause(&self, days: u32) -> Result<UpdatesStatus, UpdatesError> {
        let now = (self.inner.clock)();
        self.change(|state| {
            match self.write_updater(state, |u| {
                u.paused_until = (days > 0).then(|| now + i64::from(days) * 86_400);
            }) {
                // A saved pause or resume is the outcome now: no hold,
                // and an unreadable file has been replaced.
                Ok(()) => {
                    state.held = false;
                    state.unreadable = false;
                    Ok(())
                }
                // Nothing installs without `<data>/bin` anyway, and
                // retrying won't make one: no hold.
                Err(SaveError::NoBin) => Err(UpdatesError::BinUnavailable),
                Err(SaveError::Io | SaveError::Unreadable) => {
                    if days > 0 {
                        state.held = true;
                    }
                    Err(UpdatesError::NotSaved)
                }
            }
        })?;
        Ok(self.status())
    }

    pub fn clear_history(&self) -> Result<UpdatesStatus, UpdatesError> {
        if let Some(bin) = &self.inner.bin {
            let _state = lock(&self.inner.updater);
            bin.remove_acknowledged(history::FILE, &*self.inner.fault)
                .map_err(|e| {
                    unsaved(history::FILE, e);
                    UpdatesError::NotSaved
                })?;
        }
        Ok(self.status())
    }

    /// For `/system`'s Updates row: whether updates are on, and when the
    /// last check completed.
    pub fn summary(&self) -> (bool, Option<i64>) {
        (self.scheduled_on(), self.updater().checked_at)
    }
}

fn release_infos(
    plan: &policy::Plan,
    releases: &[lodestar_update::doc::Release],
) -> Vec<ReleaseInfo> {
    plan.releases
        .iter()
        .filter_map(|a| {
            let r = releases.iter().find(|r| r.version == a.version)?;
            Some(ReleaseInfo {
                version: r.version.to_string(),
                date: r.date as i64,
                level: match r.level {
                    lodestar_update::doc::Level::Fix => "fix",
                    lodestar_update::doc::Level::Feature => "feature",
                    lodestar_update::doc::Level::Major => "major",
                }
                .into(),
                security: r.security,
                manual_steps: r.manual_steps,
                migrates: r.migrates,
                reindex: r.reindex,
                reindex_bytes: r.reindex_bytes,
                notes: r.notes.clone(),
                notes_url: r.notes_url.clone(),
                eligible: a.eligible,
                blocked_by: a.blocked_by.iter().map(blocked).collect(),
                installs_at: a.installs_at,
            })
        })
        .collect()
}

fn blocked(b: &Block) -> Blocked {
    let (code, until, via, reason) = match b {
        Block::Mode => ("mode", None, None, None),
        Block::Level => ("level", None, None, None),
        Block::ManualSteps => ("manual_steps", None, None, None),
        Block::Reindex => ("reindex", None, None, None),
        Block::Wait { until } => ("wait", Some(*until), None, None),
        Block::Paused { until } => ("paused", Some(*until), None, None),
        Block::Held => ("held", None, None, None),
        Block::NotCurrent => ("not_current", None, None, None),
        Block::UpgradePath { via } => ("upgrade_path", None, via.clone(), None),
        Block::Capability { reason } => ("capability", None, None, Some(reason.code().into())),
    };
    Blocked {
        code: code.into(),
        until,
        via,
        reason,
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Off => "off",
        Mode::Notify => "notify",
        Mode::Auto => "auto",
        Mode::Image => "image",
    }
}

fn level_name(s: &Updates) -> &'static str {
    use crate::settings::updates::Level;
    match s.level {
        Level::Fixes => "fixes",
        Level::Features => "features",
        Level::All => "all",
    }
}

fn channel_name(s: &Updates) -> &'static str {
    s.channel.update().name()
}

fn outcome_name(o: history::Outcome) -> &'static str {
    match o {
        history::Outcome::Installed => "installed",
        history::Outcome::RolledBack => "rolled_back",
        history::Outcome::Failed => "failed",
        history::Outcome::Skipped => "skipped",
    }
}

/// Whether a check's result still counts now: current when it was
/// checked, and its manifest (or freshness) not expired since. A kept
/// result lapses on its own while later checks fail.
fn current(c: &Checked, now: i64) -> bool {
    c.current && (now.max(0) as u64) < c.expires
}

/// Runs `f` on the blocking pool (file I/O under `<data>/bin`).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Whether `dir` is on a `noexec` mount; `None` when `statvfs` fails.
/// rustix has `ST_NOEXEC` only on Linux, the one shipped Unix target;
/// elsewhere (macOS, a developer's machine) it isn't checked.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn noexec(dir: &std::path::Path) -> Option<bool> {
    rustix::fs::statvfs(dir)
        .ok()
        .map(|s| s.f_flag.contains(rustix::fs::StatVfsMountFlags::NOEXEC))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn noexec(_dir: &std::path::Path) -> Option<bool> {
    Some(false)
}

/// A random minute within the hour after the 6 hours.
fn jitter() -> i64 {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    i64::from(u16::from_le_bytes(b) % 3600)
}

async fn sleep(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

fn unix_now() -> i64 {
    crate::unix_now()
}

/// Logs a durable write that failed (the file's name and the error's
/// kind, never a path) and passes the error on.
fn unsaved(file: &'static str, e: io::Error) -> SaveError {
    log_event!(
        UPDATE_STATE_UNSAVED,
        file = file,
        error = lodestar_log::Error::io(&e)
    );
    SaveError::Io
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl UpdatesApi for Engine {
    // Each reads files under `<data>/bin` (and `statvfs`): off the
    // async workers.
    fn status(&self) -> BoxFuture<'_, UpdatesStatus> {
        let e = self.clone();
        Box::pin(blocking(move || e.status()))
    }

    fn check(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(self.check_now())
    }

    fn install(
        &self,
        version: String,
        when: InstallWhen,
        confirm_manual: bool,
    ) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(self.install_request_owned(version, when, confirm_manual))
    }

    fn skip(&self, version: String) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        let e = self.clone();
        Box::pin(blocking(move || e.skip(&version)))
    }

    fn pause(&self, days: u32) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        let e = self.clone();
        Box::pin(blocking(move || e.pause(days)))
    }

    fn clear_history(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        let e = self.clone();
        Box::pin(blocking(move || e.clear_history()))
    }

    fn rollback(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(self.rollback_request())
    }
}

#[cfg(test)]
mod tests {
    use lodestar_update::Channel;
    use lodestar_update::testing::{TestTrust, bytes, envelope, manifest};

    use super::super::check::tests::Fake;
    use super::*;

    const NOW: i64 = 1_800_000_000;
    /// The binary every test release ships.
    const BINARY: &[u8] = b"binary";

    /// Where `version`'s binary is, as `testing::manifest` names it.
    fn artifact(version: &str) -> String {
        format!("https://github.com/dvortsis/lodestar-v2/releases/download/v{version}/lodestar")
    }
    const STABLE: &str =
        "https://github.com/dvortsis/lodestar-v2/releases/download/update-manifest/stable.json";

    struct Env {
        dir: tempfile::TempDir,
        fake: Arc<Fake>,
        trust: Arc<TestTrust>,
        settings: watch::Sender<Updates>,
        events: Arc<Mutex<Vec<UpdatesEvent>>>,
        /// The engine's clock (Unix seconds); starts at [`NOW`].
        clock: Arc<std::sync::atomic::AtomicI64>,
        /// Called at each step of a durable write; passes by default.
        fault: Arc<Mutex<Hook>>,
        /// The engine's data folder (`dir`, or the one it was given).
        data: PathBuf,
        /// Where downloads come from.
        downloads: Arc<super::super::download::tests::Fake>,
        /// What keeps an install waiting.
        busy: Arc<Mutex<Option<&'static str>>>,
        /// The versions a switch was asked for.
        switched: Arc<Mutex<Vec<String>>>,
        /// The rollbacks asked for (the torrents each loses).
        rollbacks: Arc<Mutex<Vec<Option<u64>>>>,
        /// Runs when a staged overlay is verified; its result is the
        /// verification's when `Some(false)` (a failure) is returned.
        on_verify: Arc<Mutex<VerifyHook>>,
        engine: Engine,
    }

    type VerifyHook = Box<dyn Fn() -> Option<bool> + Send>;

    type Hook = Arc<dyn Fn(Step) -> io::Result<()> + Send + Sync>;

    /// The injected error: its message names a path, which must never
    /// reach a response or the log.
    fn injected() -> io::Error {
        io::Error::new(
            io::ErrorKind::StorageFull,
            "/zq7secret/data/bin/updater.json",
        )
    }

    fn env(mode: Mode) -> Env {
        env_in(mode, None)
    }

    /// An engine whose data folder is `data` (a temporary folder when
    /// `None`).
    fn env_in(mode: Mode, data: Option<PathBuf>) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let data = data.unwrap_or_else(|| dir.path().to_path_buf());
        let fake = Arc::new(Fake::default());
        let trust = Arc::new(TestTrust::default());
        let (settings, rx) = watch::channel(Updates {
            mode,
            ..Updates::default()
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let keys = trust.clone();
        let clock = Arc::new(std::sync::atomic::AtomicI64::new(NOW));
        let time = clock.clone();
        let passes: Hook = Arc::new(|_| Ok(()));
        let fault = Arc::new(Mutex::new(passes));
        let hook = fault.clone();
        let downloads = Arc::new(super::super::download::tests::Fake::default());
        let busy = Arc::new(Mutex::new(None));
        let switched = Arc::new(Mutex::new(Vec::new()));
        let rollbacks = Arc::new(Mutex::new(Vec::new()));
        let (b, sw, verify_keys) = (busy.clone(), switched.clone(), trust.clone());
        let rb = rollbacks.clone();
        let on_verify: Arc<Mutex<VerifyHook>> = Arc::new(Mutex::new(Box::new(|| None)));
        let hook_verify = on_verify.clone();
        let installer = Installer {
            source: downloads.clone(),
            busy: Box::new(move || *lock(&b)),
            switch: Box::new(move |v| lock(&sw).push(v.to_string())),
            target: Some(lodestar_update::Target::X86_64LinuxMusl),
            // The supervisor's verification, with the test keys.
            verify: Box::new(move |bin, v| {
                if let Some(result) = (lock(&hook_verify))() {
                    return result;
                }
                let dir = bin.overlay(v);
                let read = |name| std::fs::read(dir.join(name)).ok();
                let (Some(m), Some(sig), Some(binary)) = (
                    read(super::super::overlay::MANIFEST),
                    read(super::super::overlay::SIGNATURE),
                    read(super::super::overlay::BINARY),
                ) else {
                    return false;
                };
                lodestar_update::overlay(
                    &m,
                    &String::from_utf8_lossy(&sig),
                    lodestar_update::Sha256::of(&binary),
                    binary.len() as u64,
                    v,
                    lodestar_update::Target::X86_64LinuxMusl,
                    &verify_keys.embedded(vec![]),
                )
                .is_ok()
            }),
            restarting: Box::new(|| false),
            rollback: Box::new(move |lost| lock(&rb).push(lost)),
        };
        let engine = Engine::with(
            Context {
                data_dir: data.clone(),
                running: Version::new(0, 5, 0),
                image: Version::new(0, 5, 0),
                supervised: true,
                notices: vec![],
                repository: "https://github.com/dvortsis/lodestar-v2".into(),
                db: dir.path().join("lodestar.db"),
                dev: false,
            },
            rx,
            Arc::new(move |e| lock(&sink).push(e)),
            fake.clone(),
            (jiff::tz::TimeZone::UTC, "UTC".into()),
            Seams {
                clock: Box::new(move || time.load(std::sync::atomic::Ordering::Relaxed)),
                keys: Box::new(move || Ok(keys.embedded(vec![]))),
                fault: Box::new(move |step| {
                    let f = lock(&hook).clone();
                    f(step)
                }),
            },
            installer,
        );
        Env {
            dir,
            fake,
            trust,
            settings,
            events,
            clock,
            fault,
            data,
            downloads,
            busy,
            switched,
            rollbacks,
            on_verify,
            engine,
        }
    }

    impl Env {
        /// Serves a stable manifest signed by `key` (the release key by
        /// default), dated `ts`, with one release.
        fn serve(&self, ts: i64, version: &str, key: Option<&lodestar_update::testing::TestKey>) {
            self.serve_with(ts, version, key, |_| {});
        }

        /// [`Env::serve`] with the manifest edited by `edit`; the release's
        /// binary is served at its artifact URL.
        fn serve_with(
            &self,
            ts: i64,
            version: &str,
            key: Option<&lodestar_update::testing::TestKey>,
            edit: impl FnOnce(&mut serde_json::Value),
        ) {
            let mut m = manifest(Channel::Stable, ts as u64, version, BINARY);
            edit(&mut m);
            let body = bytes(&m);
            let signed = key.unwrap_or(&self.trust.release).signed(&body);
            self.fake.serve(STABLE, 200, None, &envelope(&signed, None));
            self.downloads.serve(&artifact(version), 200, None, BINARY);
        }

        fn switched(&self) -> Vec<String> {
            lock(&self.switched).clone()
        }

        /// Waits (at most 10 s) until `done`.
        async fn until(&self, what: &str, mut done: impl FnMut(&Env) -> bool) {
            let start = Instant::now();
            while !done(self) {
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "{what}: {:?}",
                    self.states()
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }

        fn set_clock(&self, t: i64) {
            self.clock.store(t, std::sync::atomic::Ordering::Relaxed);
        }

        fn trust_mark(&self) -> Option<u64> {
            let text = std::fs::read(self.dir.path().join("bin").join(trust_file::FILE)).ok()?;
            let v: serde_json::Value = serde_json::from_slice(&text).ok()?;
            v["channels"]["stable"]["hwm_ts"].as_u64()
        }

        fn states(&self) -> Vec<UpdatesState> {
            lock(&self.events).iter().map(|e| e.state).collect()
        }

        fn hook(&self, f: impl Fn(Step) -> io::Result<()> + Send + Sync + 'static) {
            *lock(&self.fault) = Arc::new(f);
        }

        /// Every durable write fails at `step`.
        fn fail_at(&self, step: Step) {
            self.hook(move |s| if s == step { Err(injected()) } else { Ok(()) });
        }

        fn heal(&self) {
            self.hook(|_| Ok(()));
        }

        fn bin(&self) -> PathBuf {
            self.data.join("bin")
        }

        /// `updater.json`'s bytes, if it exists.
        fn updater_bytes(&self) -> Option<Vec<u8>> {
            std::fs::read(self.bin().join(UPDATER_FILE)).ok()
        }

        /// A fresh engine over the same data folder: a restart.
        fn restart(&self) -> Env {
            env_in(Mode::Auto, Some(self.data.clone()))
        }
    }

    #[tokio::test]
    async fn a_good_check_lists_the_release_and_advances_trust() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        assert_eq!(e.engine.run_check().await, Ok(()));
        let s = e.engine.status();
        assert_eq!(s.status.state, CheckState::Ok);
        assert_eq!(s.status.checked_at, Some(NOW));
        let next = s.status.next_check_at.unwrap();
        assert!((NOW + 6 * 3600..NOW + 7 * 3600).contains(&next), "{next}");
        assert_eq!(s.releases.len(), 1);
        assert_eq!(s.releases[0].version, "0.6.0");
        assert!(
            !s.releases[0].eligible,
            "notify installs nothing but security fixes"
        );
        assert_eq!(e.trust_mark(), Some((NOW - 100) as u64));
        assert_eq!(
            e.states(),
            [UpdatesState::Checking, UpdatesState::Available],
            "one event per change"
        );
        assert!(s.capability.can_install, "{:?}", s.capability);
        // Only the updater's agent went out, once.
        assert_eq!(*e.fake.asked.lock().unwrap(), [STABLE]);
    }

    #[tokio::test]
    async fn a_replay_or_a_foreign_key_fails_and_moves_nothing() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        // An older manifest than the mark.
        e.serve(NOW - 200, "0.6.1", None);
        let code = e.engine.run_check().await.unwrap_err();
        assert!(!code.is_empty());
        assert_eq!(e.trust_mark(), Some((NOW - 100) as u64));
        let s = e.engine.status();
        assert_eq!(s.status.state, CheckState::Failed);
        assert_eq!(s.status.error.as_deref(), Some(code));
        // Stable signed by the nightly key.
        e.serve(NOW - 50, "0.6.2", Some(&e.trust.nightly));
        assert!(e.engine.run_check().await.is_err());
        assert_eq!(e.trust_mark(), Some((NOW - 100) as u64));
        assert_eq!(e.states().last(), Some(&UpdatesState::Failed));
    }

    /// A manifest whose signature isn't over its body fails, as does one
    /// listing only versions older than the running one (a downgrade
    /// offers nothing); neither moves the mark.
    #[tokio::test]
    async fn a_bad_signature_fails_and_a_downgrade_offers_nothing() {
        let e = env(Mode::Auto);
        let body = bytes(&manifest(
            Channel::Stable,
            (NOW - 100) as u64,
            "0.6.0",
            b"binary",
        ));
        let other = bytes(&manifest(
            Channel::Stable,
            (NOW - 100) as u64,
            "0.9.9",
            b"binary",
        ));
        let mut forged = e.trust.release.signed(&other);
        forged.minisig = e.trust.release.signed(&body).minisig;
        e.fake.serve(STABLE, 200, None, &envelope(&forged, None));
        assert_eq!(e.engine.run_check().await, Err("bad_signature"));
        assert_eq!(e.trust_mark(), None, "nothing was written");
        assert!(e.engine.status().releases.is_empty());

        e.serve(NOW - 50, "0.4.0", None);
        assert_eq!(e.engine.run_check().await, Ok(()));
        let s = e.engine.status();
        assert!(s.releases.is_empty(), "{:?}", s.releases);
        assert!(!e.states().contains(&UpdatesState::Available));
    }

    /// A manifest past its expiry (no freshness) still shows what's out,
    /// but nothing installs from it: `manifest_lapsed`, and each release
    /// is blocked as `not_current`.
    #[tokio::test]
    async fn an_expired_manifest_installs_nothing() {
        let e = env(Mode::Auto);
        e.serve(NOW - 8 * 86_400, "0.6.0", None);
        assert_eq!(e.engine.run_check().await, Ok(()));
        let s = e.engine.status();
        assert!(
            s.notices.iter().any(|n| n == "manifest_lapsed"),
            "{:?}",
            s.notices
        );
        assert_eq!(s.releases.len(), 1);
        assert!(!s.releases[0].eligible);
        assert!(
            s.releases[0]
                .blocked_by
                .iter()
                .any(|b| b.code == "not_current"),
            "{:?}",
            s.releases[0].blocked_by
        );
    }

    /// A failed check logs its code and nothing else: no URL, no host, no
    /// version.
    #[tokio::test]
    async fn a_failed_check_logs_only_its_code() {
        crate::test_log::install();
        let e = env(Mode::Notify);
        e.fake.serve(
            STABLE,
            302,
            Some("https://zq7evil.example/m.json?token=Zq7UpdateSecret"),
            b"",
        );
        assert_eq!(e.engine.run_check().await, Err("redirect_refused"));
        // Every test's events are in the capture: this one's is the line
        // with its code, and its secrets appear nowhere.
        let text = crate::test_log::text();
        assert!(
            text.lines()
                .any(|l| l.contains("update.check_failed") && l.contains("redirect_refused")),
            "{text}"
        );
        for secret in ["Zq7UpdateSecret", "zq7evil.example"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }

    /// An overlay folder planted under `<data>/bin` (a manifest dated far
    /// ahead, signed by a key the build doesn't trust) raises no mark: only
    /// a verified overlay's mark counts, so it can't freeze updates.
    #[tokio::test]
    async fn a_planted_overlay_raises_no_mark() {
        use std::os::unix::fs::PermissionsExt;

        let e = env(Mode::Notify);
        let planted = e.dir.path().join("bin").join("0.9.0");
        std::fs::create_dir_all(&planted).unwrap();
        let body = bytes(&manifest(
            Channel::Stable,
            (NOW + 86_400) as u64,
            "0.9.0",
            b"binary",
        ));
        let forged = lodestar_update::testing::TestKey::new(77).signed(&body);
        std::fs::write(planted.join(super::super::overlay::MANIFEST), &body).unwrap();
        std::fs::write(
            planted.join(super::super::overlay::SIGNATURE),
            forged.minisig.as_bytes(),
        )
        .unwrap();
        let binary = planted.join(super::super::overlay::BINARY);
        std::fs::write(&binary, b"binary").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();

        e.serve(NOW - 100, "0.6.0", None);
        assert_eq!(e.engine.run_check().await, Ok(()));
        assert_eq!(e.trust_mark(), Some((NOW - 100) as u64));
        assert_eq!(e.engine.status().releases.len(), 1);
    }

    /// A data folder whose `bin` can't be opened (here, a file in its
    /// place): the check fails without a request, and the next one is
    /// still six hours away, so the schedule doesn't check back to back.
    #[tokio::test]
    async fn without_a_bin_folder_the_schedule_still_waits() {
        let scratch = tempfile::tempdir().unwrap();
        let data = scratch.path().join("data");
        std::fs::write(&data, b"not a folder").unwrap();
        let e = env_in(Mode::Notify, Some(data));
        e.serve(NOW - 100, "0.6.0", None);
        assert_eq!(e.engine.until_scheduled(), Some(Duration::ZERO));
        assert_eq!(e.engine.run_check().await, Err("bin_unavailable"));
        assert!(e.fake.asked.lock().unwrap().is_empty(), "nothing fetched");
        let wait = e.engine.until_scheduled().unwrap();
        assert!(wait >= EVERY, "{wait:?}");
        let s = e.engine.status();
        assert_eq!(s.status.checked_at, Some(NOW));
        assert_eq!(s.status.error.as_deref(), Some("bin_unavailable"));
        assert!(
            !s.notices.contains(&"check_not_saved".to_string()),
            "bin_unavailable already says it: {:?}",
            s.notices
        );
    }

    /// A good check's result lapses at its manifest's expiry even while
    /// every later check fails: `manifest_lapsed`, and nothing is current.
    #[tokio::test]
    async fn a_kept_result_lapses_at_its_expiry() {
        let e = env(Mode::Auto);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let s = e.engine.status();
        assert!(
            !s.notices.iter().any(|n| n == "manifest_lapsed"),
            "{:?}",
            s.notices
        );
        assert!(
            !s.releases[0]
                .blocked_by
                .iter()
                .any(|b| b.code == "not_current"),
            "{:?}",
            s.releases[0].blocked_by
        );
        // The refetches fail from here on; the manifest expires 7 days in.
        e.fake.serve(STABLE, 500, None, b"");
        e.set_clock(NOW + 8 * 86_400);
        assert!(e.engine.run_check().await.is_err());
        let s = e.engine.status();
        assert_eq!(s.status.state, CheckState::Failed);
        assert!(
            s.notices.iter().any(|n| n == "manifest_lapsed"),
            "{:?}",
            s.notices
        );
        assert!(
            s.releases[0]
                .blocked_by
                .iter()
                .any(|b| b.code == "not_current"),
            "{:?}",
            s.releases[0].blocked_by
        );
    }

    /// The clock jumps both ways: a next check left far ahead by a clock
    /// that ran fast (or an engine that started at 1970) is due now, and
    /// one left behind by a clock that jumped forward is due too.
    #[tokio::test]
    async fn the_schedule_survives_the_clock_jumping() {
        let e = env(Mode::Notify);
        // A clock 334 days fast schedules the next check then.
        e.set_clock(NOW + 334 * 86_400);
        e.engine.run_check().await.unwrap();
        e.set_clock(NOW);
        assert_eq!(
            e.engine.until_scheduled(),
            Some(Duration::ZERO),
            "corrected: due now"
        );
        e.engine.run_check().await.unwrap();
        let wait = e.engine.until_scheduled().unwrap();
        assert!(
            wait >= EVERY && wait <= EVERY + Duration::from_secs(3600),
            "{wait:?}"
        );
        // A board that booted at 1970 sees a 2026 schedule as far ahead.
        e.set_clock(0);
        assert_eq!(e.engine.until_scheduled(), Some(Duration::ZERO));
        // Forward past the next check: due.
        e.set_clock(NOW + 30 * 86_400);
        assert_eq!(e.engine.until_scheduled(), Some(Duration::ZERO));
    }

    /// An unreadable `skipped` blocks automatic installs instead of
    /// forgetting a skip.
    #[tokio::test]
    async fn an_unreadable_skip_list_blocks_installs() {
        let e = env(Mode::Auto);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        assert_ne!(
            e.engine.status().capability.reason.as_deref(),
            Some("skipped_unreadable")
        );
        std::fs::write(e.dir.path().join("bin").join("skipped"), b"not json").unwrap();
        let s = e.engine.status();
        assert_eq!(s.capability.reason.as_deref(), Some("skipped_unreadable"));
        assert!(!s.releases[0].eligible);
    }

    /// Install now: downloaded, staged, verified as the supervisor will,
    /// verified again, then the switch is asked for, with one event per
    /// step.
    #[tokio::test]
    async fn install_now_stages_verifies_and_switches() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let s = e
            .engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        assert!(s.capability.can_install, "{:?}", s.capability);
        e.until("switched", |e| !e.switched().is_empty()).await;
        assert_eq!(e.switched(), ["0.6.0"]);
        let states = e.states();
        let at = |s| {
            states
                .iter()
                .position(|x| *x == s)
                .unwrap_or_else(|| panic!("{s:?} in {states:?}"))
        };
        assert!(at(UpdatesState::Downloading) < at(UpdatesState::Verifying));
        assert!(at(UpdatesState::Verifying) < at(UpdatesState::Installing));
        assert!(at(UpdatesState::Installing) < at(UpdatesState::Restarting));
        let bin = Bin::open(e.dir.path()).unwrap();
        assert_eq!(bin.overlays().unwrap(), [Version::new(0, 6, 0)]);
        assert_eq!(
            std::fs::read(bin.overlay(&Version::new(0, 6, 0)).join("lodestar")).unwrap(),
            BINARY
        );
        assert_eq!(*e.downloads.asked.lock().unwrap(), [artifact("0.6.0")]);
    }

    #[tokio::test]
    async fn install_now_refuses_what_it_mustnt_install() {
        let e = env(Mode::Notify);
        e.serve_with(NOW - 100, "0.6.0", None, |m| {
            m["releases"][0]["manual_steps"] = true.into();
        });
        e.engine.run_check().await.unwrap();
        let ask = |v: &'static str, confirm| e.engine.install_request(v, InstallWhen::Now, confirm);
        assert_eq!(
            ask("9.9.9", false).await.unwrap_err(),
            UpdatesError::UnknownVersion
        );
        assert_eq!(
            ask("not a version", false).await.unwrap_err(),
            UpdatesError::UnknownVersion
        );
        assert_eq!(
            ask("0.6.0", false).await.unwrap_err(),
            UpdatesError::ManualConfirmationRequired
        );
        e.engine.skip("0.6.0").unwrap();
        assert_eq!(
            ask("0.6.0", true).await.unwrap_err(),
            UpdatesError::VersionSkipped
        );
        assert!(e.switched().is_empty());
    }

    /// A download that isn't the pinned binary, or a rebuild under way,
    /// fails the install: `failed` with the code, no overlay, no switch.
    #[tokio::test]
    async fn a_bad_download_or_a_rebuild_fails_the_install() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        // Same size, another body: the hash catches it.
        e.downloads.serve(&artifact("0.6.0"), 200, None, b"binarY");
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("failed", |e| e.states().contains(&UpdatesState::Failed))
            .await;
        let failed = lock(&e.events).last().cloned().unwrap();
        assert_eq!(failed.error.as_deref(), Some("download_mismatch"));
        assert!(e.switched().is_empty());
        assert!(
            Bin::open(e.dir.path())
                .unwrap()
                .overlays()
                .unwrap()
                .is_empty()
        );

        *lock(&e.busy) = Some("rebuild_running");
        e.downloads.serve(&artifact("0.6.0"), 200, None, BINARY);
        lock(&e.events).clear();
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("failed", |e| e.states().contains(&UpdatesState::Failed))
            .await;
        assert_eq!(
            lock(&e.events).last().unwrap().error.as_deref(),
            Some("rebuild_running")
        );
        assert!(e.switched().is_empty());
    }

    /// `when: window` queues it: due at the window's opening, not before.
    #[tokio::test]
    async fn a_queued_install_waits_for_the_window() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let s = e
            .engine
            .install_request("0.6.0", InstallWhen::Window, false)
            .await
            .unwrap();
        assert_eq!(
            s.installing,
            Some(Installing {
                version: "0.6.0".into(),
                when: InstallWhen::Window
            })
        );
        assert_eq!(e.states().last(), Some(&UpdatesState::WaitingWindow));
        // NOW is 08:00 UTC; the window is 03:00-05:00.
        let (v, due) = e.engine.next_install().unwrap();
        assert_eq!((v, due), (Version::new(0, 6, 0), false));
        let opens = NOW - 8 * 3600 + 86_400 + 3 * 3600;
        assert_eq!(
            e.engine.until_install(),
            Some(Duration::from_secs((opens - NOW) as u64))
        );
        e.set_clock(opens + 60);
        assert_eq!(e.engine.next_install(), Some((Version::new(0, 6, 0), true)));
    }

    /// Install automatically: an eligible release is due at its
    /// `installs_at`; once it fails it waits for the next check.
    #[tokio::test]
    async fn an_automatic_install_is_due_in_the_window_and_not_retried() {
        let e = env(Mode::Auto);
        e.serve(NOW - 5 * 86_400, "0.6.0", None);
        e.set_clock(NOW - 5 * 86_400 + 100);
        e.engine.run_check().await.unwrap();
        e.set_clock(NOW);
        let (v, _) = e.engine.next_install().expect("an automatic install");
        assert_eq!(v, Version::new(0, 6, 0));
        let one = e.engine.inner.checking.clone().lock_owned().await;
        e.downloads.serve(&artifact("0.6.0"), 500, None, b"");
        assert!(e.engine.run_install(one, v).await.is_err());
        assert_eq!(
            e.engine.next_install(),
            None,
            "not retried before the next check"
        );
    }

    /// A version in probation announces `checking_health` at start; a
    /// rollback the history records is announced once, not again.
    #[tokio::test]
    async fn the_start_announces_probation_and_a_rollback_once() {
        let e = env(Mode::Notify);
        let bin = Bin::open(e.dir.path()).unwrap();
        bin.write(
            "pending",
            &pending::Pending {
                format: pending::PENDING_FORMAT,
                version: "0.5.0".into(),
                previous: "0.4.9".into(),
                kind: pending::Kind::Install,
                attempts: 1,
                used_ms: 0,
                index_before: vec![],
            },
        )
        .unwrap();
        history::append(
            &bin,
            history::Entry {
                version: "0.5.1".into(),
                from: "0.5.0".into(),
                level: None,
                at: 1_000,
                result: history::Outcome::RolledBack,
                reason: Some("selftest".into()),
                lost_torrents: None,
                journal: Some(1_000),
            },
        )
        .unwrap();
        e.engine.announce_start().await;
        assert_eq!(
            e.states(),
            [UpdatesState::CheckingHealth, UpdatesState::RolledBack]
        );
        lock(&e.events).clear();
        // A restart: the rollback was announced; probation still runs.
        e.engine.announce_start().await;
        assert_eq!(e.states(), [UpdatesState::CheckingHealth]);
    }

    #[tokio::test]
    async fn a_damaged_trust_file_blocks_installs_and_is_never_overwritten() {
        let e = env(Mode::Auto);
        std::fs::create_dir_all(e.dir.path().join("bin")).unwrap();
        let path = e.dir.path().join("bin").join(trust_file::FILE);
        std::fs::write(&path, b"{ damaged").unwrap();
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let s = e.engine.status();
        assert!(
            s.notices.contains(&"trust_damaged".to_string()),
            "{:?}",
            s.notices
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"{ damaged");
        assert_eq!(s.releases.len(), 1, "the check still shows what's out");
    }

    /// Damaged trust data refuses Install now and starts no download.
    #[tokio::test]
    async fn a_damaged_trust_file_refuses_install_now() {
        let e = env(Mode::Notify);
        std::fs::create_dir_all(e.bin()).unwrap();
        std::fs::write(e.bin().join(trust_file::FILE), b"{ damaged").unwrap();
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        for when in [InstallWhen::Now, InstallWhen::Window] {
            assert_eq!(
                e.engine.install_request("0.6.0", when, false).await,
                Err(UpdatesError::InstallUnavailable("trust_damaged"))
            );
        }
        assert!(e.downloads.asked.lock().unwrap().is_empty());
        assert_eq!(e.engine.status().installing, None);
    }

    /// Image mode (as `LODESTAR_UPDATES=image` locks it) refuses Install
    /// now; a second Install now while one runs is `busy`, and a check is
    /// rate limited meanwhile.
    #[tokio::test]
    async fn image_mode_and_an_install_under_way_refuse_another() {
        let e = env(Mode::Image);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        assert_eq!(
            e.engine
                .install_request("0.6.0", InstallWhen::Now, false)
                .await,
            Err(UpdatesError::InstallUnavailable("image_mode"))
        );
        e.settings.send_modify(|s| s.mode = Mode::Notify);
        // The one-check-or-install lock, as a running install holds it.
        let one = e.engine.inner.checking.clone().lock_owned().await;
        assert_eq!(
            e.engine
                .install_request("0.6.0", InstallWhen::Now, false)
                .await,
            Err(UpdatesError::InstallUnavailable("busy"))
        );
        assert!(matches!(
            e.engine.check_now().await,
            Err(UpdatesError::RateLimited { .. })
        ));
        drop(one);
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("switched", |e| !e.switched().is_empty()).await;
        assert_eq!(e.switched(), ["0.6.0"], "one install, one switch");
    }

    /// Queuing for the window is acknowledged only once it's durable
    /// (DUR-26), and never writes over an unreadable `updater.json`.
    #[tokio::test]
    async fn a_queued_install_is_saved_or_refused() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let before = e.updater_bytes();
        e.fail_at(Step::Rename);
        assert_eq!(
            e.engine
                .install_request("0.6.0", InstallWhen::Window, false)
                .await,
            Err(UpdatesError::NotSaved)
        );
        assert_eq!(e.updater_bytes(), before);
        assert_eq!(e.engine.status().installing, None);
        e.heal();
        e.engine
            .install_request("0.6.0", InstallWhen::Window, false)
            .await
            .unwrap();
        let restarted = e.restart();
        assert_eq!(restarted.engine.updater().queued.as_deref(), Some("0.6.0"));

        let scratch = env(Mode::Notify);
        std::fs::create_dir_all(scratch.bin()).unwrap();
        std::fs::write(scratch.bin().join(UPDATER_FILE), b"{ damaged").unwrap();
        let u = scratch.restart();
        u.serve(NOW - 100, "0.6.0", None);
        u.engine.run_check().await.unwrap();
        assert_eq!(
            u.engine
                .install_request("0.6.0", InstallWhen::Window, false)
                .await,
            Err(UpdatesError::InstallUnavailable("updater_unreadable"))
        );
        assert_eq!(u.updater_bytes().unwrap(), b"{ damaged");
    }

    /// Leftovers of an install that crashed (a `.part`, a `.tmp` folder, a
    /// stale staging of the same version) don't stop the next one, and
    /// the staged binary is the pinned one.
    #[tokio::test]
    async fn an_install_after_a_crash_replaces_its_leftovers() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let bin = Bin::open(&e.data).unwrap();
        std::fs::write(bin.path("0.6.0.part"), b"half").unwrap();
        std::fs::create_dir(bin.path("0.6.0.tmp")).unwrap();
        std::fs::write(bin.path("0.6.0.tmp/lodestar"), b"half").unwrap();
        std::fs::create_dir(bin.path("0.6.0")).unwrap();
        std::fs::write(bin.path("0.6.0/lodestar"), b"stale").unwrap();
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("switched", |e| !e.switched().is_empty()).await;
        let v = Version::new(0, 6, 0);
        assert_eq!(
            std::fs::read(bin.overlay(&v).join("lodestar")).unwrap(),
            BINARY
        );
        assert!(!bin.path("0.6.0.part").exists());
        assert!(!bin.path("0.6.0.tmp").exists());
    }

    /// The last install's `failed` code, with no switch asked for and no
    /// overlay left behind.
    async fn install_fails_with(e: &Env, code: &str) {
        lock(&e.events).clear();
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("failed", |e| e.states().contains(&UpdatesState::Failed))
            .await;
        let failed = lock(&e.events).last().cloned().unwrap();
        assert_eq!(failed.error.as_deref(), Some(code));
        assert!(e.switched().is_empty(), "{code}: no switch");
        assert!(
            Bin::open(&e.data).unwrap().overlays().unwrap().is_empty(),
            "{code}: the staged folder is removed"
        );
        assert_eq!(e.engine.status().installing, None);
    }

    /// A staged overlay that the supervisor's verification would refuse.
    #[tokio::test]
    async fn a_staged_overlay_that_fails_verification_is_removed() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        *lock(&e.on_verify) = Box::new(|| Some(false));
        install_fails_with(&e, "bad_overlay").await;
    }

    /// The manifest verified again after staging: an expiry passed, or a
    /// `trust.json` damaged since, refuses the switch.
    #[tokio::test]
    async fn the_second_verification_refuses_an_expired_manifest_or_damaged_trust() {
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        // During the overlay's verification, after the first one passed:
        // the clock moves past the manifest's expiry (7 days).
        let clock = e.clock.clone();
        *lock(&e.on_verify) = Box::new(move || {
            clock.store(NOW + 8 * 86_400, std::sync::atomic::Ordering::Relaxed);
            None
        });
        install_fails_with(&e, "not_current").await;

        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let trust = e.bin().join(trust_file::FILE);
        *lock(&e.on_verify) = Box::new(move || {
            std::fs::write(&trust, b"{ damaged").unwrap();
            None
        });
        install_fails_with(&e, "trust_damaged").await;
    }

    /// No room for the binary, the release's index rebuild and the margin.
    #[tokio::test]
    async fn no_room_on_the_disk_fails_before_the_download() {
        let e = env(Mode::Notify);
        e.serve_with(NOW - 100, "0.6.0", None, |m| {
            m["releases"][0]["reindex_bytes"] = (u64::MAX / 2).into();
        });
        e.engine.run_check().await.unwrap();
        install_fails_with(&e, "no_space").await;
        assert!(e.downloads.asked.lock().unwrap().is_empty());
    }

    /// Once a switch is asked for, nothing installs, checks or stages
    /// again before the process exits.
    #[tokio::test]
    async fn after_a_switch_nothing_installs_again() {
        let e = env(Mode::Auto);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("switched", |e| !e.switched().is_empty()).await;
        let staged = Bin::open(&e.data).unwrap().overlay(&Version::new(0, 6, 0));
        let before = std::fs::metadata(&staged).unwrap().modified().unwrap();
        assert_eq!(
            e.engine
                .install_request("0.6.0", InstallWhen::Now, false)
                .await,
            Err(UpdatesError::InstallUnavailable("busy"))
        );
        assert!(matches!(
            e.engine.check_now().await,
            Err(UpdatesError::RateLimited { .. })
        ));
        let s = e.engine.status();
        assert_eq!(
            s.installing,
            Some(Installing {
                version: "0.6.0".into(),
                when: InstallWhen::Now
            })
        );
        // The schedule stays out too, though its next check is due.
        e.set_clock(NOW + 8 * 3600);
        let (stop, stopped) = watch::channel(false);
        let task = e.engine.spawn(stopped);
        e.until("still one switch", |e| {
            e.engine.inner.switching.lock().unwrap().is_some()
        })
        .await;
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the schedule stops while the switch holds the lock")
            .unwrap();
        assert_eq!(e.switched(), ["0.6.0"]);
        assert_eq!(
            std::fs::metadata(&staged).unwrap().modified().unwrap(),
            before
        );
    }

    /// `<data>/bin`'s entries (names), sorted.
    fn bin_names(e: &Env) -> Vec<String> {
        let mut n: Vec<_> = std::fs::read_dir(e.bin())
            .unwrap()
            .map(|d| d.unwrap().file_name().into_string().unwrap())
            .collect();
        n.sort();
        n
    }

    /// The path `version`'s artifact is served at.
    fn artifact_path(version: &str) -> String {
        format!("/dvortsis/lodestar-v2/releases/download/v{version}/lodestar")
    }

    /// The release source over real HTTP on loopback (the allowlist sees
    /// the signed GitHub URLs): a wrong hash, a wrong size, an oversized
    /// body and another target's artifact each fail with their stable
    /// code and leave nothing in `<data>/bin` (no `.part`, `.tmp` or
    /// overlay); then the right bytes install.
    #[tokio::test]
    async fn a_loopback_source_refuses_each_bad_artifact_and_leaves_nothing() {
        let e = env(Mode::Notify);
        // The release also ships an aarch64 binary, pinned at its own URL.
        let arm: &[u8] = b"arm64!";
        e.serve_with(NOW - 100, "0.6.0", None, |m| {
            let mut other = m["releases"][0]["artifacts"][0].clone();
            other["target"] = "aarch64-unknown-linux-musl".into();
            other["url"] = (artifact("0.6.0") + "-arm64").into();
            other["sha256"] = lodestar_update::Sha256::of(arm).to_hex().into();
            m["releases"][0]["artifacts"]
                .as_array_mut()
                .unwrap()
                .push(other);
        });
        let server = super::super::download::tests::Loopback::start().await;
        *lock(&e.downloads.forward) = Some(server.base.clone());
        e.engine.run_check().await.unwrap();
        // `Bin::open` made it; the check wrote its trust state.
        let clean = bin_names(&e);
        let path = artifact_path("0.6.0");
        for (what, body, code) in [
            ("a wrong hash", b"binarY".to_vec(), "download_mismatch"),
            ("a wrong size", b"binar".to_vec(), "download_mismatch"),
            ("oversized", vec![0u8; 100_000], "download_too_large"),
            ("another target's", arm.to_vec(), "download_mismatch"),
        ] {
            server.serve(&path, 200, None, &body);
            lock(&e.events).clear();
            e.engine
                .install_request("0.6.0", InstallWhen::Now, false)
                .await
                .unwrap();
            e.until(what, |e| e.states().contains(&UpdatesState::Failed))
                .await;
            let failed = lock(&e.events).last().cloned().unwrap();
            assert_eq!(failed.error.as_deref(), Some(code), "{what}");
            assert_eq!(bin_names(&e), clean, "{what}: nothing left in bin");
            assert!(e.switched().is_empty(), "{what}");
        }
        server.serve(&path, 200, None, BINARY);
        lock(&e.events).clear();
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("switched", |e| !e.switched().is_empty()).await;
        // It went over HTTP, with the updater's request and nothing more.
        let heads = server.heads.lock().unwrap().clone();
        assert_eq!(heads.len(), 5, "{heads:?}");
        for h in &heads {
            assert!(h.starts_with(&format!("GET {path} HTTP/1.1")), "{h}");
            assert!(h.contains("Lodestar-Updater"), "{h}");
            assert!(!h.to_ascii_lowercase().contains("cookie"), "{h}");
        }
    }

    /// A manifest with no artifact for this build's target installs
    /// nothing and downloads nothing.
    #[tokio::test]
    async fn a_release_for_another_target_only_is_refused_before_the_download() {
        let e = env(Mode::Notify);
        e.serve_with(NOW - 100, "0.6.0", None, |m| {
            m["releases"][0]["artifacts"][0]["target"] = "aarch64-unknown-linux-musl".into();
        });
        e.engine.run_check().await.unwrap();
        let clean = bin_names(&e);
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("failed", |e| e.states().contains(&UpdatesState::Failed))
            .await;
        let failed = lock(&e.events).last().cloned().unwrap();
        assert_eq!(failed.error.as_deref(), Some("no_artifact"));
        assert!(e.downloads.asked.lock().unwrap().is_empty());
        assert_eq!(bin_names(&e), clean);
    }

    /// The log over a whole install against the loopback source. The
    /// first attempt is redirected, with a credential in the URL, to a host
    /// off the allowlist: `update.install_failed` with `redirect_refused`,
    /// a line no other engine test logs, so it's this install's. The second
    /// goes through a signed asset URL carrying a signature and a
    /// credential, and installs. No URL, host, query or credential appears.
    #[tokio::test]
    async fn the_log_over_an_install_names_events_and_never_urls() {
        crate::test_log::install();
        let e = env(Mode::Notify);
        e.serve(NOW - 100, "0.6.0", None);
        let server = super::super::download::tests::Loopback::start().await;
        *lock(&e.downloads.forward) = Some(server.base.clone());
        let path = artifact_path("0.6.0");
        server.serve(
            &path,
            302,
            Some("https://zq7evil.example/lodestar?token=Zq7EvilSecret"),
            b"",
        );
        e.engine.run_check().await.unwrap();
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("failed", |e| e.states().contains(&UpdatesState::Failed))
            .await;
        let failed = lock(&e.events).last().cloned().unwrap();
        assert_eq!(failed.error.as_deref(), Some("redirect_refused"));

        let asset = "/zq7asset/lodestar?sig=Zq7InstallSecret&X-Amz-Credential=zq7cred";
        server.serve(
            &path,
            302,
            Some(&format!(
                "https://release-assets.githubusercontent.com{asset}"
            )),
            b"",
        );
        server.serve(asset, 200, None, BINARY);
        lock(&e.events).clear();
        e.engine
            .install_request("0.6.0", InstallWhen::Now, false)
            .await
            .unwrap();
        e.until("switched", |e| !e.switched().is_empty()).await;

        // Every test's events are in the capture: the failed attempt is
        // the only `install_failed` line with `redirect_refused`.
        let text = crate::test_log::text();
        let failures: Vec<&str> = text
            .lines()
            .filter(|l| {
                l.contains(r#"event="update.install_failed""#) && l.contains("redirect_refused")
            })
            .collect();
        assert_eq!(failures.len(), 1, "{text}");
        assert!(failures[0].contains("error=redirect_refused"), "{text}");
        // The credentials appear nowhere, not even in the HTTP client's
        // own trace lines (third-party lines are capped at warnings and
        // scrubbed in production anyway).
        for secret in ["Zq7EvilSecret", "zq7evil", "Zq7InstallSecret", "zq7cred"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        // Lodestar's own events name no URL, host or address.
        let ours: Vec<&str> = text.lines().filter(|l| l.contains("event=")).collect();
        let port = server.base.port().unwrap().to_string();
        for secret in [
            "zq7asset",
            "release-assets.githubusercontent.com",
            "github.com",
            "https://",
            &format!("127.0.0.1:{port}"),
        ] {
            for l in &ours {
                assert!(!l.contains(secret), "{secret} in {l}");
            }
        }
    }

    /// `active` naming the running version, the image before it.
    fn activate(e: &Env) {
        Bin::open(&e.data)
            .unwrap()
            .write(
                "active",
                &pending::Active {
                    version: "0.5.0".into(),
                    previous: pending::IMAGE.into(),
                },
            )
            .unwrap();
    }

    /// Roll back is offered only with a previous version; asking for it
    /// asks the supervisor once (the torrents it loses, when counted),
    /// keeps the one-check lock until the restart, and refuses a second
    /// rollback, an install and a check meanwhile.
    #[tokio::test]
    async fn rollback_is_offered_then_asked_for_once() {
        let e = env(Mode::Notify);
        let s = e.engine.status();
        assert!(!s.rollback.available);
        assert_eq!(s.rollback.reason.as_deref(), Some("no_previous_version"));
        assert_eq!(
            e.engine.rollback_request().await.unwrap_err(),
            UpdatesError::RollbackUnavailable("no_previous_version")
        );

        activate(&e);
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let s = e.engine.status();
        assert!(s.rollback.available, "{:?}", s.rollback);
        assert_eq!(s.rollback.reason, None);
        assert_eq!(s.rollback.to_version.as_deref(), Some("image"));

        let s = e.engine.rollback_request().await.unwrap();
        assert_eq!(*lock(&e.rollbacks), [None]);
        assert!(!s.rollback.available);
        assert_eq!(s.rollback.reason.as_deref(), Some("busy"));
        assert!(e.states().contains(&UpdatesState::Restarting));
        assert_eq!(
            e.engine.rollback_request().await.unwrap_err(),
            UpdatesError::RollbackUnavailable("busy")
        );
        assert_eq!(
            e.engine
                .install_request("0.6.0", InstallWhen::Now, false)
                .await
                .unwrap_err(),
            UpdatesError::InstallUnavailable("busy")
        );
        assert!(e.engine.check_now().await.is_err());
        assert_eq!(lock(&e.rollbacks).len(), 1, "asked once");
        assert!(e.switched().is_empty());
    }

    /// Image mode, a check or an install holding the lock, and a migrated
    /// store whose snapshot is gone each refuse with their reason, and
    /// ask nothing of the supervisor.
    #[tokio::test]
    async fn rollback_is_refused_in_image_mode_while_busy_or_without_its_snapshot() {
        let e = env(Mode::Image);
        activate(&e);
        assert_eq!(
            e.engine.status().rollback.reason.as_deref(),
            Some("image_mode")
        );
        assert_eq!(
            e.engine.rollback_request().await.unwrap_err(),
            UpdatesError::RollbackUnavailable("image_mode")
        );

        let e = env(Mode::Notify);
        activate(&e);
        let held = e.engine.inner.checking.clone().try_lock_owned().unwrap();
        assert_eq!(e.engine.status().rollback.reason.as_deref(), Some("busy"));
        assert_eq!(
            e.engine.rollback_request().await.unwrap_err(),
            UpdatesError::RollbackUnavailable("busy")
        );
        drop(held);
        assert!(e.engine.status().rollback.available);

        // 0.5.0 migrated the store from the schema before this one, and
        // no snapshot of that is left.
        drop(
            lodestar_store::Store::open(
                e.dir.path().join("lodestar.db"),
                lodestar_store::StoreConfig::default(),
            )
            .unwrap(),
        );
        let overlay = Bin::open(&e.data).unwrap().overlay(&Version::new(0, 5, 0));
        std::fs::create_dir_all(&overlay).unwrap();
        let latest = lodestar_store::latest_schema_version();
        std::fs::write(
            overlay.join("migration.json"),
            serde_json::json!({"from": latest - 1, "to": latest, "started_at": 1}).to_string(),
        )
        .unwrap();
        assert_eq!(
            e.engine.status().rollback.reason.as_deref(),
            Some("snapshot_missing")
        );
        assert_eq!(
            e.engine.rollback_request().await.unwrap_err(),
            UpdatesError::RollbackUnavailable("snapshot_missing")
        );
        assert!(lock(&e.rollbacks).is_empty());
    }

    /// Crash safety of the route's entry: before the graceful shutdown
    /// writes `request.json`, asking writes nothing under `<data>/bin`
    /// (no journal, `active` as it was), so a crash there changes nothing
    /// and the next start offers Roll back again. From `request.json` on
    /// it's the supervisor's journal (`tests/supervisor.rs`).
    #[tokio::test]
    async fn a_crash_before_the_shutdown_leaves_nothing_to_finish() {
        let e = env(Mode::Notify);
        activate(&e);
        let names = |e: &Env| {
            let mut n: Vec<_> = std::fs::read_dir(e.bin())
                .unwrap()
                .map(|d| d.unwrap().file_name().into_string().unwrap())
                .collect();
            n.sort();
            n
        };
        let before = names(&e);
        e.engine.rollback_request().await.unwrap();
        assert_eq!(names(&e), before);
        assert!(!e.bin().join(journal::FILE).exists());
        let again = e.restart();
        assert!(again.engine.status().rollback.available);
    }

    #[tokio::test]
    async fn nothing_published_is_no_error() {
        let e = env(Mode::Notify);
        assert_eq!(e.engine.run_check().await, Ok(()));
        let s = e.engine.status();
        assert_eq!((s.status.state, s.releases.len()), (CheckState::Ok, 0));
        assert_eq!(
            e.states(),
            [UpdatesState::Checking, UpdatesState::Checked],
            "every check ends with an event"
        );
    }

    #[tokio::test]
    async fn the_manual_check_is_rate_limited() {
        let e = env(Mode::Off);
        e.serve(NOW - 100, "0.6.0", None);
        let s = e.engine.check_now().await.unwrap();
        assert_eq!(s.releases.len(), 1, "a manual check runs even when off");
        assert_eq!(
            s.status.next_check_at, None,
            "nothing is scheduled when off"
        );
        assert!(matches!(
            e.engine.check_now().await,
            Err(UpdatesError::RateLimited { retry_after }) if (1..=60).contains(&retry_after)
        ));
    }

    /// DUR-26, queued-save ordering: an older check save is held inside
    /// its write while a newer pause queues behind it. Released, the
    /// check lands first and the pause is written over it, so the pause
    /// is acknowledged and survives a restart with the check's times.
    #[test]
    fn a_queued_pause_lands_after_an_older_save_and_survives_restart() {
        use std::sync::mpsc;
        let e = env(Mode::Auto);
        let (writing, started) = mpsc::channel();
        let (release, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(Some(wait));
        let writing = Mutex::new(writing);
        // The first write (the check's) stops midway until released.
        e.hook(move |step| {
            if step == Step::Write
                && let Some(w) = lock(&wait).take()
            {
                lock(&writing).send(()).unwrap();
                w.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            Ok(())
        });
        let old = e.engine.clone();
        let check = std::thread::spawn(move || {
            old.save_check(CheckTimes {
                checked_at: NOW,
                next_check_at: NOW + 3600,
                error: None,
            });
        });
        started.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            matches!(
                e.engine.inner.updater.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "the state stays locked through the write"
        );
        // Readers don't wait on the write: the scheduler and status read
        // the published view from async workers.
        let reader = e.engine.clone();
        let (read, reading) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = reader.until_scheduled();
            let _ = reader.summary();
            let _ = reader.status();
            read.send(()).unwrap();
        });
        reading
            .recv_timeout(Duration::from_secs(10))
            .expect("a reader waited on the write's lock");
        let newer = e.engine.clone();
        let (pausing, attempting) = mpsc::channel();
        let pause = std::thread::spawn(move || {
            pausing.send(()).unwrap();
            newer.pause(7)
        });
        attempting.recv_timeout(Duration::from_secs(10)).unwrap();
        release.send(()).unwrap();
        check.join().unwrap();
        let status = pause.join().unwrap().expect("the pause was saved");
        assert_eq!(status.policy.paused_until, Some(NOW + 7 * 86_400));
        assert!(!status.policy.held_in_process);
        assert!(!status.notices.contains(&"check_not_saved".to_string()));
        let persisted: Updater = serde_json::from_slice(&e.updater_bytes().unwrap()).unwrap();
        assert_eq!(persisted, e.engine.updater());
        assert_eq!(persisted.checked_at, Some(NOW));
        assert_eq!(persisted.paused_until, Some(NOW + 7 * 86_400));
        assert_eq!(e.restart().engine.updater(), persisted);
        // A later check keeps the pause, and a saved resume survives too.
        e.engine.save_check(CheckTimes {
            checked_at: NOW + 1,
            next_check_at: NOW + 7200,
            error: None,
        });
        e.engine.pause(0).unwrap();
        let restarted = e.restart();
        assert_eq!(restarted.engine.updater().paused_until, None);
        assert_eq!(restarted.engine.updater().checked_at, Some(NOW + 1));
    }

    /// DUR-26: a write, flush or rename that fails before the rename
    /// leaves the old durable file as it was, fails the request, leaves
    /// no temporary file, and a restart loads the old state, not a
    /// partial one (a planted half-written temporary file included).
    #[tokio::test]
    async fn a_failed_write_flush_or_rename_keeps_the_old_file() {
        for step in [Step::Write, Step::Flush, Step::Rename] {
            let e = env(Mode::Auto);
            e.engine.pause(1).unwrap();
            let old = e.updater_bytes().unwrap();
            e.fail_at(step);
            assert_eq!(e.engine.pause(30), Err(UpdatesError::NotSaved), "{step:?}");
            assert_eq!(e.updater_bytes().unwrap(), old, "{step:?}");
            let names: Vec<_> = std::fs::read_dir(e.bin())
                .unwrap()
                .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                !names.iter().any(|n| n.ends_with(".tmp")),
                "{step:?}: {names:?}"
            );
            let s = e.engine.status();
            assert_eq!(s.policy.paused_until, Some(NOW + 86_400), "{step:?}");
            assert!(s.policy.held_in_process, "{step:?}");
            // A crash midway would leave half a file beside the old one.
            std::fs::write(e.bin().join(".updater.json.tmp"), &old[..old.len() / 2]).unwrap();
            let restarted = e.restart();
            assert_eq!(
                restarted.engine.updater().paused_until,
                Some(NOW + 86_400),
                "{step:?}"
            );
            assert!(
                !restarted.engine.status().policy.held_in_process,
                "{step:?}"
            );
        }
    }

    /// DUR-26: a pause that fails is refused, holds automatic installs in
    /// this process (eligibility itself, not just the response), and is
    /// never written as a pause: a restart from the old file isn't paused.
    #[tokio::test]
    async fn a_failed_pause_holds_in_process_only() {
        let e = env(Mode::Auto);
        e.serve(NOW - 10 * 86_400, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let settings = e.settings.borrow().clone();
        let before = e.updater_bytes();
        e.fail_at(Step::Rename);
        assert_eq!(e.engine.pause(7), Err(UpdatesError::NotSaved));
        assert_eq!(e.updater_bytes(), before, "nothing was written");
        let plan = e.engine.plan(&settings, NOW);
        assert!(plan.releases.iter().all(|a| !a.eligible));
        assert!(plan.releases[0].blocked_by.contains(&Block::Held));
        assert_eq!(plan.next, None);
        let s = e.engine.status();
        assert!(s.policy.held_in_process);
        assert_eq!(s.policy.paused_until, None, "the hold isn't a pause");
        assert!(s.releases[0].blocked_by.iter().any(|b| b.code == "held"));
        // A later check doesn't lift the hold or write it.
        e.heal();
        e.set_clock(NOW + 60);
        e.engine.run_check().await.unwrap();
        assert!(e.engine.status().policy.held_in_process);
        let written: Updater = serde_json::from_slice(&e.updater_bytes().unwrap()).unwrap();
        assert_eq!(written.paused_until, None);
        let restarted = e.restart();
        assert_eq!(restarted.engine.updater().paused_until, None);
        assert!(!restarted.engine.status().policy.held_in_process);
    }

    /// DUR-26: a failed resume keeps the durable pause and the hold; only
    /// a later saved resume ends both, and that survives a restart.
    #[tokio::test]
    async fn a_failed_resume_keeps_the_pause_and_the_hold() {
        let e = env(Mode::Auto);
        e.engine.pause(7).unwrap();
        e.fail_at(Step::Flush);
        assert_eq!(e.engine.pause(30), Err(UpdatesError::NotSaved));
        assert_eq!(e.engine.pause(0), Err(UpdatesError::NotSaved));
        let s = e.engine.status();
        assert_eq!(s.policy.paused_until, Some(NOW + 7 * 86_400));
        assert!(s.policy.held_in_process);
        assert_eq!(
            e.restart().engine.updater().paused_until,
            Some(NOW + 7 * 86_400)
        );
        e.heal();
        let s = e.engine.pause(0).unwrap();
        assert_eq!(s.policy.paused_until, None);
        assert!(!s.policy.held_in_process);
        assert_eq!(e.restart().engine.updater().paused_until, None);
    }

    /// DUR-26, the background-check race: a check whose save is held and
    /// then fails, a pause that fails behind it, and a retried pause that
    /// succeeds. The newest durable pause stands, the check's times stay
    /// in memory only (`check_not_saved`), and the log names the file
    /// and the error's kind, never a path.
    #[test]
    fn a_failed_check_save_never_overwrites_a_durable_pause() {
        use std::sync::mpsc;
        crate::test_log::install();
        let e = env(Mode::Auto);
        e.engine.pause(1).unwrap();
        let (writing, started) = mpsc::channel();
        let (release, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(Some(wait));
        let writing = Mutex::new(writing);
        // Every write fails; the first (the check's) waits to be released.
        e.hook(move |step| {
            if step == Step::Write
                && let Some(w) = lock(&wait).take()
            {
                lock(&writing).send(()).unwrap();
                w.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            if step == Step::Rename {
                return Err(injected());
            }
            Ok(())
        });
        let old = e.engine.clone();
        let check = std::thread::spawn(move || {
            old.save_check(CheckTimes {
                checked_at: NOW,
                next_check_at: NOW + 3600,
                error: None,
            });
        });
        started.recv_timeout(Duration::from_secs(10)).unwrap();
        let newer = e.engine.clone();
        let (pausing, attempting) = mpsc::channel();
        let pause = std::thread::spawn(move || {
            pausing.send(()).unwrap();
            newer.pause(30)
        });
        attempting.recv_timeout(Duration::from_secs(10)).unwrap();
        release.send(()).unwrap();
        check.join().unwrap();
        assert_eq!(pause.join().unwrap(), Err(UpdatesError::NotSaved));
        let s = e.engine.status();
        assert!(s.policy.held_in_process);
        assert_eq!(s.policy.paused_until, Some(NOW + 86_400));
        assert_eq!(s.status.checked_at, Some(NOW), "shown, though unsaved");
        assert!(s.notices.contains(&"check_not_saved".to_string()));
        // The retry is a new request, saved on its own.
        e.heal();
        let s = e.engine.pause(7).unwrap();
        assert_eq!(s.policy.paused_until, Some(NOW + 7 * 86_400));
        assert!(!s.policy.held_in_process);
        assert!(s.notices.contains(&"check_not_saved".to_string()));
        let written: Updater = serde_json::from_slice(&e.updater_bytes().unwrap()).unwrap();
        assert_eq!(written.paused_until, Some(NOW + 7 * 86_400));
        assert_eq!(written.checked_at, None, "the unsaved check isn't written");
        let restarted = e.restart();
        assert_eq!(restarted.engine.updater(), written);
        // A check saved later clears the notice and keeps the pause.
        e.engine.save_check(CheckTimes {
            checked_at: NOW + 5,
            next_check_at: NOW + 3600,
            error: None,
        });
        assert!(
            !e.engine
                .status()
                .notices
                .contains(&"check_not_saved".to_string())
        );
        let restarted = e.restart();
        assert_eq!(
            restarted.engine.updater().paused_until,
            Some(NOW + 7 * 86_400)
        );
        assert_eq!(restarted.engine.updater().checked_at, Some(NOW + 5));
        let text = crate::test_log::text();
        assert!(
            text.lines()
                .any(|l| l.contains("update.state_unsaved") && l.contains("updater.json")),
            "{text}"
        );
        assert!(!text.contains("zq7secret"), "{text}");
    }

    /// DUR-26's accepted residual: a folder fsync that fails after the
    /// rename fails the request, but the new file is already in place, so
    /// a restart may find it. A failed pause errs toward paused; a failed
    /// resume may come back resumed. In this process both keep the
    /// contract (hold on, pause kept).
    #[tokio::test]
    async fn a_failed_folder_fsync_fails_the_request_with_a_known_residual() {
        let e = env(Mode::Auto);
        e.fail_at(Step::SyncDir);
        assert_eq!(e.engine.pause(7), Err(UpdatesError::NotSaved));
        assert!(e.engine.status().policy.held_in_process);
        assert_eq!(e.engine.status().policy.paused_until, None);
        assert_eq!(
            e.restart().engine.updater().paused_until,
            Some(NOW + 7 * 86_400),
            "the renamed file may survive: toward paused"
        );

        let e = env(Mode::Auto);
        e.engine.pause(7).unwrap();
        e.fail_at(Step::SyncDir);
        assert_eq!(e.engine.pause(0), Err(UpdatesError::NotSaved));
        let s = e.engine.status();
        assert_eq!(s.policy.paused_until, Some(NOW + 7 * 86_400));
        assert!(!s.policy.held_in_process);
        assert_eq!(
            e.restart().engine.updater().paused_until,
            None,
            "the residual: the resume may survive the restart"
        );
    }

    /// A filesystem that can't fsync a folder at all (`EINVAL` or
    /// `ENOTSUP`, as on Unraid's FUSE `/mnt/user`) leaves the rename
    /// standing: the change is acknowledged, and that's logged once per
    /// run. `EIO` there is a real failure: `updates_not_saved`.
    #[tokio::test]
    async fn an_unsupported_folder_fsync_is_acknowledged_but_eio_is_not() {
        crate::test_log::install();
        let bin_of = |e: &Env| e.engine.inner.bin.clone().unwrap();
        for errno in [rustix::io::Errno::INVAL, rustix::io::Errno::NOTSUP] {
            let e = env(Mode::Auto);
            e.hook(move |s| {
                if s == Step::SyncDir {
                    Err(errno.into())
                } else {
                    Ok(())
                }
            });
            let s = e.engine.pause(7).unwrap();
            assert_eq!(s.policy.paused_until, Some(NOW + 7 * 86_400), "{errno:?}");
            assert!(!s.policy.held_in_process, "{errno:?}");
            assert!(bin_of(&e).dir_sync_unsupported(), "{errno:?}");
            e.engine.pause(0).unwrap();
            assert_eq!(e.restart().engine.updater().paused_until, None, "{errno:?}");
        }
        // Logged once per run however many saves meet it: one line per
        // engine above (two), not one per save (four).
        let text = crate::test_log::text();
        let lines = text
            .lines()
            .filter(|l| l.contains("update.dir_sync_unsupported"))
            .count();
        assert_eq!(lines, 2, "{text}");

        let e = env(Mode::Auto);
        e.hook(|s| {
            if s == Step::SyncDir {
                Err(rustix::io::Errno::IO.into())
            } else {
                Ok(())
            }
        });
        assert_eq!(e.engine.pause(7), Err(UpdatesError::NotSaved));
        assert!(e.engine.status().policy.held_in_process);
        assert!(!bin_of(&e).dir_sync_unsupported());
    }

    /// Without `<data>/bin`, a pause or resume is `bin_unavailable`
    /// (nothing can install there anyway) and turns on no hold.
    #[tokio::test]
    async fn without_a_bin_folder_a_pause_is_refused_without_a_hold() {
        let scratch = tempfile::tempdir().unwrap();
        let data = scratch.path().join("data");
        std::fs::write(&data, b"not a folder").unwrap();
        let e = env_in(Mode::Auto, Some(data));
        assert_eq!(e.engine.pause(7), Err(UpdatesError::BinUnavailable));
        assert_eq!(e.engine.pause(0), Err(UpdatesError::BinUnavailable));
        let s = e.engine.status();
        assert!(!s.policy.held_in_process);
        assert_eq!(s.policy.paused_until, None);
    }

    /// DUR-26: a skip or a history clear that can't be saved is
    /// `updates_not_saved`, and the files are as they were.
    #[tokio::test]
    async fn a_failed_skip_or_history_clear_is_not_saved() {
        let e = env(Mode::Auto);
        e.serve(NOW - 10 * 86_400, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let history =
            br#"{"entries":[{"version":"0.5.0","from":"image","at":1,"result":"installed"}]}"#;
        std::fs::write(e.bin().join(history::FILE), history).unwrap();
        e.fail_at(Step::Rename);
        assert_eq!(e.engine.skip("0.6.0"), Err(UpdatesError::NotSaved));
        assert!(!e.bin().join(pending::SKIPPED).exists());
        assert_eq!(e.engine.status().releases.len(), 1, "not skipped");
        assert_eq!(e.engine.clear_history(), Err(UpdatesError::NotSaved));
        assert_eq!(std::fs::read(e.bin().join(history::FILE)).unwrap(), history);
        e.heal();
        assert!(e.engine.skip("0.6.0").unwrap().releases.is_empty());
        assert!(e.engine.clear_history().unwrap().history.is_empty());
        assert!(!e.bin().join(history::FILE).exists());
    }

    /// The residual for files read from disk: a skip or clear whose
    /// folder fsync alone fails is refused, though the renamed (or
    /// removed) file is already in effect, as the route docs say.
    #[tokio::test]
    async fn a_skip_or_clear_failing_at_the_folder_fsync_is_refused_but_may_apply() {
        let e = env(Mode::Auto);
        e.serve(NOW - 10 * 86_400, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        std::fs::write(
            e.bin().join(history::FILE),
            br#"{"entries":[{"version":"0.5.0","from":"image","at":1,"result":"installed"}]}"#,
        )
        .unwrap();
        e.fail_at(Step::SyncDir);
        assert_eq!(e.engine.skip("0.6.0"), Err(UpdatesError::NotSaved));
        assert!(e.engine.status().releases.is_empty(), "already in effect");
        assert_eq!(e.engine.clear_history(), Err(UpdatesError::NotSaved));
        assert!(e.engine.status().history.is_empty(), "already in effect");
    }

    /// An unreadable skip list is damage, not a failed save: no
    /// `updates_not_saved`, and nothing is written over it.
    #[tokio::test]
    async fn an_unreadable_skip_list_is_not_a_failed_save() {
        let e = env(Mode::Auto);
        e.serve(NOW - 10 * 86_400, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        std::fs::write(e.bin().join(pending::SKIPPED), b"[{").unwrap();
        assert!(matches!(
            e.engine.skip("0.6.0"),
            Err(UpdatesError::Internal(_))
        ));
        assert_eq!(
            std::fs::read(e.bin().join(pending::SKIPPED)).unwrap(),
            b"[{"
        );
    }

    /// An `updater.json` that's there but can't be read may hold a
    /// pause: the engine fails closed. Installs are held, a check's times
    /// stay in memory and never write over the file, and only an
    /// explicit pause or resume replaces it and ends the hold.
    #[tokio::test]
    async fn an_unreadable_updater_file_holds_and_is_never_written_over_by_a_check() {
        let scratch = env(Mode::Auto);
        std::fs::create_dir_all(scratch.bin()).unwrap();
        std::fs::write(scratch.bin().join(UPDATER_FILE), b"{ damaged").unwrap();
        let e = scratch.restart();
        let s = e.engine.status();
        assert!(s.policy.held_in_process);
        assert!(s.notices.contains(&"updater_unreadable".to_string()));
        e.serve(NOW - 10 * 86_400, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        assert_eq!(e.updater_bytes().unwrap(), b"{ damaged", "not written over");
        let s = e.engine.status();
        assert_eq!(s.status.checked_at, Some(NOW), "kept in memory");
        assert!(s.notices.contains(&"check_not_saved".to_string()));
        assert!(
            e.engine
                .plan(&e.settings.borrow().clone(), NOW)
                .releases
                .iter()
                .all(|a| !a.eligible)
        );
        // A failed resume changes nothing; a saved one ends it all.
        e.fail_at(Step::Rename);
        assert_eq!(e.engine.pause(0), Err(UpdatesError::NotSaved));
        assert!(e.engine.status().policy.held_in_process);
        e.heal();
        let s = e.engine.pause(0).unwrap();
        assert!(!s.policy.held_in_process);
        assert!(!s.notices.contains(&"updater_unreadable".to_string()));
        let restarted = e.restart();
        assert!(!restarted.engine.status().policy.held_in_process);
        assert_eq!(restarted.engine.updater().paused_until, None);
    }

    /// The way out of `updater_unreadable`: a saved explicit pause
    /// rewrites the file and ends the hold, and the next start reads the
    /// new file normally (paused, no hold, no notice), after which checks
    /// are saved again.
    #[tokio::test]
    async fn a_saved_pause_replaces_an_unreadable_updater_file() {
        let scratch = env(Mode::Auto);
        std::fs::create_dir_all(scratch.bin()).unwrap();
        std::fs::write(scratch.bin().join(UPDATER_FILE), b"\x00\xff not json").unwrap();
        let e = scratch.restart();
        assert!(e.engine.status().policy.held_in_process);
        let s = e.engine.pause(7).unwrap();
        assert!(!s.policy.held_in_process);
        assert_eq!(s.policy.paused_until, Some(NOW + 7 * 86_400));
        let written: Updater = serde_json::from_slice(&e.updater_bytes().unwrap()).unwrap();
        assert_eq!(written.paused_until, Some(NOW + 7 * 86_400));
        let next = e.restart();
        let s = next.engine.status();
        assert!(!s.policy.held_in_process);
        assert!(!s.notices.contains(&"updater_unreadable".to_string()));
        assert_eq!(s.policy.paused_until, Some(NOW + 7 * 86_400));
        next.engine.run_check().await.unwrap();
        let written: Updater = serde_json::from_slice(&next.updater_bytes().unwrap()).unwrap();
        assert_eq!(written.checked_at, Some(NOW), "checks are saved again");
        assert_eq!(written.paused_until, Some(NOW + 7 * 86_400));
    }

    #[tokio::test]
    async fn skip_pause_and_clear_write_their_state() {
        let e = env(Mode::Auto);
        e.serve(NOW - 10 * 86_400, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        assert_eq!(e.engine.skip("0.9.9"), Err(UpdatesError::UnknownVersion));
        let s = e.engine.skip("0.6.0").unwrap();
        assert!(s.releases.is_empty(), "a skipped version isn't a candidate");
        let skipped: serde_json::Value = serde_json::from_slice(
            &std::fs::read(e.dir.path().join("bin").join("skipped")).unwrap(),
        )
        .unwrap();
        assert_eq!(skipped[0]["reason"], "user");

        let s = e.engine.pause(7).unwrap();
        assert_eq!(s.policy.paused_until, Some(NOW + 7 * 86_400));
        assert_eq!(e.engine.pause(0).unwrap().policy.paused_until, None);

        std::fs::write(
            e.dir.path().join("bin").join(history::FILE),
            br#"{"entries":[{"version":"0.5.0","from":"image","at":1,"result":"rolled_back"}]}"#,
        )
        .unwrap();
        let s = e.engine.status();
        assert_eq!(s.history.len(), 1);
        assert!(s.notices.contains(&"rolled_back".to_string()));
        assert!(e.engine.clear_history().unwrap().history.is_empty());
    }

    #[tokio::test]
    async fn the_schedule_follows_the_mode() {
        let e = env(Mode::Off);
        assert_eq!(e.engine.until_scheduled(), None);
        e.settings.send_modify(|s| s.mode = Mode::Notify);
        assert_eq!(
            e.engine.until_scheduled(),
            Some(Duration::ZERO),
            "due at once"
        );
        e.serve(NOW - 100, "0.6.0", None);
        e.engine.run_check().await.unwrap();
        let wait = e.engine.until_scheduled().unwrap();
        assert!(wait >= EVERY && wait < EVERY + Duration::from_secs(3600));
        assert_eq!(e.engine.summary(), (true, Some(NOW)));
    }
}
