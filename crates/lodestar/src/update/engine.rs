//! The update engine (§12.2) in `serve --child`: the scheduled and manual
//! signed checks, `updater.json`, the policy over what a check found, and
//! everything `/updates` and `/system` report. Part 4 of B-update checks,
//! decides and reports; installing comes in part 5, so `can_install` is
//! `not_built_yet` wherever nothing else stops it.
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
//! - **Events:** `checking` when a check starts, `available` when it
//!   found a candidate, `failed` with its code.
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
    Blocked, Capability, CheckState, CheckStatus, HistoryEntry, PendingInstall, Policy,
    ReleaseInfo, RollbackInfo, Running, UpdatesApi, UpdatesError, UpdatesStatus,
};
use lodestar_log::log_event;
use lodestar_update::semver::Version;
use lodestar_update::{Checked, Notice};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use super::check::{self, CheckError, Fetch};
use super::layout::{Bin, Step};
use super::log_events::{
    UPDATE_AVAILABLE, UPDATE_CHECK_FAILED, UPDATE_OVERLAY_UNREADABLE, UPDATE_STATE_UNSAVED,
};
use super::policy::{self, Block, Reason};
use super::window::{When, Window};
use super::{history, pending, trust_file};
use crate::settings::updates::{Mode, Updates};

/// Between scheduled checks, before the random minute.
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Between manual checks.
const MANUAL_EVERY: Duration = Duration::from_secs(60);
/// The longest the schedule sleeps before it looks at the clock again,
/// so a clock that's corrected while it sleeps is noticed.
const MAX_SLEEP: Duration = Duration::from_secs(15 * 60);
pub const UPDATER_FILE: &str = "updater.json";

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
    /// A pause failed to save: automatic installs are held in this
    /// process until a pause or resume is saved. Never written.
    held: bool,
}

impl State {
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
    /// temporary file.
    updater: Mutex<State>,
    fault: Fault,
    /// One check at a time.
    checking: tokio::sync::Mutex<()>,
    last_manual: Mutex<Option<Instant>>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    /// The keys checks verify with (the embedded ones, or tests').
    keys: Box<dyn Fn() -> Result<lodestar_update::Embedded, lodestar_update::Error> + Send + Sync>,
}

/// Where `updates` events go: the event hub, or a test's list.
pub type Publish = Arc<dyn Fn(UpdatesEvent) + Send + Sync>;

/// What the last check found.
#[derive(Default)]
struct Found {
    state: Option<CheckState>,
    checked: Option<Checked>,
    notices: Vec<String>,
    trust_damaged: bool,
}

impl Engine {
    pub fn new(
        ctx: Context,
        settings: watch::Receiver<Updates>,
        hub: Arc<EventHub>,
        fetch: Arc<dyn Fetch>,
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
        )
    }

    fn with(
        ctx: Context,
        settings: watch::Receiver<Updates>,
        publish: Publish,
        fetch: Arc<dyn Fetch>,
        zone: (jiff::tz::TimeZone, String),
        Seams { clock, keys, fault }: Seams,
    ) -> Engine {
        let bin = Bin::open(&ctx.data_dir).ok();
        let durable = bin
            .as_ref()
            .and_then(|b| b.read::<Updater>(UPDATER_FILE).ok().flatten())
            .unwrap_or_default();
        Engine {
            inner: Arc::new(Inner {
                ctx,
                bin,
                settings,
                publish,
                fetch,
                zone,
                found: Mutex::new(Found::default()),
                updater: Mutex::new(State {
                    durable,
                    ..State::default()
                }),
                fault,
                checking: tokio::sync::Mutex::new(()),
                last_manual: Mutex::new(None),
                clock,
                keys,
            }),
        }
    }

    /// The scheduled checks, until `stop` is set.
    pub fn spawn(&self, mut stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        let engine = self.clone();
        let mut settings = self.inner.settings.clone();
        tokio::spawn(async move {
            loop {
                let wait = engine.until_scheduled();
                tokio::select! {
                    _ = stop.changed() => return,
                    r = settings.changed() => {
                        if r.is_err() { return; }
                        continue;
                    }
                    () = sleep(wait.map(|w| w.min(MAX_SLEEP))) => {}
                }
                if engine.due() {
                    // Still due once the lock is ours: a manual check may
                    // have run while this one waited for it.
                    let one = engine.inner.checking.lock().await;
                    if engine.due() {
                        let _ = engine.run_check_holding(one).await;
                    }
                }
            }
        })
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
        lock(&self.inner.updater).effective()
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
        let mut state = lock(&self.inner.updater);
        state.unsaved_check = match self.write_updater(&mut state, |u| times.apply(u)) {
            Ok(()) => None,
            Err(_) => Some(times),
        };
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
        let one = self.inner.checking.lock().await;
        self.run_check_holding(one).await
    }

    /// [`Engine::run_check`] with the one-at-a-time lock already held.
    async fn run_check_holding(
        &self,
        _one: tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<(), &'static str> {
        let settings = self.inner.settings.borrow().clone();
        let channel = settings.channel.update();
        self.set_state(CheckState::Checking);
        self.publish(UpdatesState::Checking, None, None);
        let result = self.check_once(channel).await;
        let now = (self.inner.clock)();
        let next = now + EVERY.as_secs() as i64 + jitter();
        self.save_check(CheckTimes {
            checked_at: now,
            next_check_at: next,
            error: result.err().map(str::to_string),
        });
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
            f.notices.clear();
            f.trust_damaged = false;
            return Ok(());
        };
        let embedded = (self.inner.keys)().map_err(lodestar_update::Error::code)?;
        let now = (self.inner.clock)();
        let checked = tokio::task::spawn_blocking(move || {
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
            if let Ok(c) = &checked {
                let _ = trust_file::save(bin.dir(), &loaded, c.next.clone());
            }
            (checked, damaged)
        })
        .await
        .map_err(|_| "internal")?;
        let (checked, damaged) = checked;
        let mut f = lock(&self.inner.found);
        f.trust_damaged = damaged;
        match checked {
            Ok(c) => {
                f.notices = c.notices.iter().map(|n| n.code().to_string()).collect();
                f.checked = Some(c);
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

    /// Why this installation can't install itself (part 4 installs
    /// nothing, so `not_built_yet` when nothing else stops it).
    fn incapable(&self, settings: &Updates, trust_damaged: bool, skipped_ok: bool) -> Reason {
        let ctx = &self.inner.ctx;
        if !ctx.supervised {
            return Reason::NotSupervised;
        }
        // Named before the rest: a broken file is a fault to fix, and it
        // blocks automatic installs rather than forgetting a skip.
        if !skipped_ok {
            return Reason::SkippedUnreadable;
        }
        if crate::system::info::channel() == lodestar_api::system::Channel::Dev {
            return Reason::DevBuild;
        }
        if std::env::var("LODESTAR_DEPLOYMENT").is_ok_and(|d| d.trim() == "package") {
            return Reason::PackageManaged;
        }
        if settings.mode == Mode::Image {
            return Reason::ImageMode;
        }
        let Some(bin) = &self.inner.bin else {
            return Reason::NotWritable;
        };
        match noexec(bin.dir()) {
            Some(true) => return Reason::Noexec,
            // Not known isn't "not noexec".
            None => return Reason::NoexecUnknown,
            Some(false) => {}
        }
        if rustix::fs::access(bin.dir(), rustix::fs::Access::WRITE_OK).is_err() {
            return Reason::NotWritable;
        }
        if trust_damaged {
            return Reason::TrustDamaged;
        }
        let keys = (self.inner.keys)().ok();
        let role = settings.channel.update().role();
        let has_key = keys.is_some_and(|k| match role {
            lodestar_update::Role::Release => k.release.is_some(),
            lodestar_update::Role::Nightly => k.nightly.is_some(),
            _ => false,
        });
        if !has_key {
            return Reason::ChannelUnavailable;
        }
        Reason::NotBuiltYet
    }

    fn plan(&self, settings: &Updates, now: i64) -> policy::Plan {
        let updater = lock(&self.inner.updater);
        let (paused_until, held_in_process) = (updater.durable.paused_until, updater.held);
        drop(updater);
        let found = lock(&self.inner.found);
        let Some(c) = &found.checked else {
            return policy::Plan::default();
        };
        let window = Window::parse(&settings.window).ok();
        let tz = self.inner.zone.0.clone();
        let window_from = move |t: i64| match window.map(|w| w.when(t, &tz)) {
            Some(When::Open { .. }) | None => t,
            Some(When::Opens(at)) => at,
        };
        let skipped = self.skipped();
        let incapable = Some(self.incapable(settings, found.trust_damaged, skipped.is_some()));
        let skipped = skipped.unwrap_or_default();
        policy::plan(&policy::Inputs {
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
        })
    }

    /// Everything `GET /updates` reports.
    pub fn status(&self) -> UpdatesStatus {
        let settings = self.inner.settings.borrow().clone();
        let now = (self.inner.clock)();
        let ctx = &self.inner.ctx;
        let (updater, check_unsaved, held_in_process) = {
            let state = lock(&self.inner.updater);
            (state.effective(), state.unsaved_check.is_some(), state.held)
        };
        let plan = self.plan(&settings, now);
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
        if check_unsaved {
            notices.push("check_not_saved".into());
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
                can_install: false,
                reason: Some(reason.code().to_string()),
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

    /// Roll back arrives with installing (part 5); until then it's never
    /// offered, though where it would go is shown.
    fn rollback(&self) -> RollbackInfo {
        let active: Option<pending::Active> = self
            .inner
            .bin
            .as_ref()
            .and_then(|b| b.read("active").ok().flatten())
            .filter(|a: &pending::Active| a.version == self.inner.ctx.running.to_string());
        RollbackInfo {
            available: false,
            to_version: active.map(|a| a.previous),
            lost_torrents: super::probation::current().and_then(|c| c.lost_torrents()),
        }
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
            let Ok(one) = self.inner.checking.try_lock() else {
                return Err(UpdatesError::RateLimited { retry_after: 5 });
            };
            *last = Some(Instant::now());
            one
        };
        let _ = self.run_check_holding(one).await;
        let e = self.clone();
        Ok(blocking(move || e.status()).await)
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
            pending::skip_with(bin, version, "user", |list| {
                bin.write_acknowledged(pending::SKIPPED, list, &*self.inner.fault)
            })
            .map_err(|e| {
                unsaved(pending::SKIPPED, e);
                UpdatesError::NotSaved
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
        {
            let mut state = lock(&self.inner.updater);
            match self.write_updater(&mut state, |u| {
                u.paused_until = (days > 0).then(|| now + i64::from(days) * 86_400);
            }) {
                // A saved pause or resume is the outcome now: no hold.
                Ok(()) => state.held = false,
                // Nothing installs without `<data>/bin` anyway, and
                // retrying won't make one: no hold.
                Err(SaveError::NoBin) => return Err(UpdatesError::BinUnavailable),
                Err(SaveError::Io) => {
                    if days > 0 {
                        state.held = true;
                    }
                    return Err(UpdatesError::NotSaved);
                }
            }
        }
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
}

#[cfg(test)]
mod tests {
    use lodestar_update::Channel;
    use lodestar_update::testing::{TestTrust, bytes, envelope, manifest};

    use super::super::check::tests::Fake;
    use super::*;

    const NOW: i64 = 1_800_000_000;
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
        engine: Engine,
    }

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
        let engine = Engine::with(
            Context {
                data_dir: data.unwrap_or_else(|| dir.path().to_path_buf()),
                running: Version::new(0, 5, 0),
                image: Version::new(0, 5, 0),
                supervised: true,
                notices: vec![],
                repository: "https://github.com/dvortsis/lodestar-v2".into(),
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
        );
        Env {
            dir,
            fake,
            trust,
            settings,
            events,
            clock,
            fault,
            engine,
        }
    }

    impl Env {
        /// Serves a stable manifest signed by `key` (the release key by
        /// default), dated `ts`, with one release.
        fn serve(&self, ts: i64, version: &str, key: Option<&lodestar_update::testing::TestKey>) {
            let body = bytes(&manifest(Channel::Stable, ts as u64, version, b"binary"));
            let signed = key.unwrap_or(&self.trust.release).signed(&body);
            self.fake.serve(STABLE, 200, None, &envelope(&signed, None));
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
            self.dir.path().join("bin")
        }

        /// `updater.json`'s bytes, if it exists.
        fn updater_bytes(&self) -> Option<Vec<u8>> {
            std::fs::read(self.bin().join(UPDATER_FILE)).ok()
        }

        /// A fresh engine over the same data folder: a restart.
        fn restart(&self) -> Env {
            env_in(Mode::Auto, Some(self.dir.path().to_path_buf()))
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
        assert!(!s.capability.can_install);
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
