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
use super::layout::Bin;
use super::log_events::{
    UPDATE_AVAILABLE, UPDATE_CHECK_FAILED, UPDATE_INSTALL_FAILED, UPDATE_OVERLAY_UNREADABLE,
};
use super::policy::{self, Block, Reason, Refusal};
use super::window::{When, Window};
use super::{download, history, install, layout, pending, trust_file};
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
    pub verify: Box<dyn Fn(&Bin, &Version) -> bool + Send + Sync>,
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
    /// `updater.json` as the engine knows it: the source of truth, which
    /// the file only keeps across restarts. Were the file the truth, a
    /// `<data>/bin` that can't be written would leave no next check, and
    /// the schedule would check back to back.
    updater: Mutex<Updater>,
    /// One check or install at a time: an install holds it throughout.
    checking: Arc<tokio::sync::Mutex<()>>,
    last_manual: Mutex<Option<Instant>>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    installer: Installer,
    /// The version an install under way is for.
    installing: Mutex<Option<Version>>,
    /// The version an automatic install last failed for: not tried again
    /// until the next check.
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
            Box::new(unix_now),
            Box::new(super::trust_root),
            installer,
        )
    }

    fn with(
        ctx: Context,
        settings: watch::Receiver<Updates>,
        publish: Publish,
        fetch: Arc<dyn Fetch>,
        zone: (jiff::tz::TimeZone, String),
        clock: Box<dyn Fn() -> i64 + Send + Sync>,
        keys: Box<
            dyn Fn() -> Result<lodestar_update::Embedded, lodestar_update::Error> + Send + Sync,
        >,
        installer: Installer,
    ) -> Engine {
        let bin = Bin::open(&ctx.data_dir).ok();
        let updater = bin
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
                updater: Mutex::new(updater),
                checking: Arc::new(tokio::sync::Mutex::new(())),
                last_manual: Mutex::new(None),
                clock,
                installer,
                installing: Mutex::new(None),
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
            engine.announce_start();
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
                if engine.due() {
                    // Still due once the lock is ours: a manual check may
                    // have run while this one waited for it.
                    let one = engine.inner.checking.clone().lock_owned().await;
                    if engine.due() {
                        let _ = engine.run_check_holding(one).await;
                    }
                }
                if engine.until_install().is_some_and(|d| d.is_zero()) {
                    let one = engine.inner.checking.clone().lock_owned().await;
                    if let Some((v, true)) = engine.next_install() {
                        let _ = engine.run_install(one, v).await;
                    }
                }
            }
        })
    }

    /// The install the schedule makes next and whether it's due now: a
    /// version queued for the window, else the policy's next automatic
    /// install (not one that failed since the last check).
    fn next_install(&self) -> Option<(Version, bool)> {
        self.install_at()
            .map(|(v, at)| (v, at <= (self.inner.clock)()))
    }

    fn install_at(&self) -> Option<(Version, i64)> {
        let settings = self.inner.settings.borrow().clone();
        let now = (self.inner.clock)();
        if let Some(q) = self
            .updater()
            .queued
            .as_deref()
            .and_then(layout::parse_version)
        {
            let at = Window::parse(&settings.window).ok().map_or(now, |w| {
                match w.when(now, &self.inner.zone.0) {
                    When::Open { .. } => now,
                    When::Opens(at) => at,
                }
            });
            return Some((q, at));
        }
        let (v, at) = self.plan(&settings, now).next?;
        let failed = lock(&self.inner.failed).as_ref() == Some(&v);
        (!failed).then_some((v, at))
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
    fn announce_start(&self) {
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
            let at = last.at;
            self.save_updater(|u| u.rolled_back_seen = Some(at));
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

    fn updater(&self) -> Updater {
        lock(&self.inner.updater).clone()
    }

    /// Changes the engine's `updater.json`, then writes it if it can: a
    /// failed write loses it only across a restart.
    fn save_updater(&self, f: impl FnOnce(&mut Updater)) {
        let u = {
            let mut u = lock(&self.inner.updater);
            u.format = 1;
            f(&mut u);
            u.clone()
        };
        if let Some(bin) = &self.inner.bin {
            let _ = bin.write(UPDATER_FILE, &u);
        }
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
        let error = result.err().map(str::to_string);
        self.save_updater(|u| {
            u.checked_at = Some(now);
            u.next_check_at = Some(next);
            u.error = error.clone();
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

    fn plan(&self, settings: &Updates, now: i64) -> policy::Plan {
        self.with_inputs(settings, now, policy::plan)
            .unwrap_or_default()
    }

    /// `f` over the policy's inputs as they are now; `None` before a check
    /// found a manifest.
    fn with_inputs<T>(
        &self,
        settings: &Updates,
        now: i64,
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
            paused_until: self.updater().paused_until,
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
        let updater = self.updater();
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
        self.may_install(&v, confirm_manual)?;
        match when {
            InstallWhen::Window => {
                self.save_updater(|u| u.queued = Some(v.to_string()));
                self.publish(UpdatesState::WaitingWindow, Some(v.to_string()), None);
                self.inner.wake.notify_one();
            }
            InstallWhen::Now => {
                let Ok(one) = self.inner.checking.clone().try_lock_owned() else {
                    return Err(UpdatesError::InstallUnavailable("busy"));
                };
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
        let manual = self
            .with_inputs(&settings, now, |i| {
                policy::install_now(i, v)?;
                Ok(i.releases.iter().any(|r| r.version == *v && r.manual_steps))
            })
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
    async fn run_install(&self, _one: OneAtATime, v: Version) -> Result<(), &'static str> {
        *lock(&self.inner.installing) = Some(v.clone());
        let result = self.install_steps(&v).await;
        *lock(&self.inner.installing) = None;
        let queued = self.updater().queued.as_deref() == Some(v.to_string().as_str());
        if queued {
            self.save_updater(|u| u.queued = None);
        }
        if let Err(code) = result {
            *lock(&self.inner.failed) = Some(v.clone());
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
        let (again, _) = self.reverify(&bin, v).await?;
        lodestar_update::install(&again, v, target, lodestar_update::ArtifactKind::Binary)
            .map_err(lodestar_update::Error::code)?;

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
        pending::skip(bin, version, "user").map_err(|e| UpdatesError::Internal(e.to_string()))?;
        Ok(self.status())
    }

    pub fn pause(&self, days: u32) -> UpdatesStatus {
        let now = (self.inner.clock)();
        self.save_updater(|u| {
            u.paused_until = (days > 0).then(|| now + i64::from(days) * 86_400);
        });
        self.status()
    }

    pub fn clear_history(&self) -> Result<UpdatesStatus, UpdatesError> {
        if let Some(bin) = &self.inner.bin {
            bin.remove(history::FILE)
                .map_err(|e| UpdatesError::Internal(e.to_string()))?;
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
        Box::pin(async move { Ok(blocking(move || e.pause(days)).await) })
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
        /// Where downloads come from.
        downloads: Arc<super::super::download::tests::Fake>,
        /// What keeps an install waiting.
        busy: Arc<Mutex<Option<&'static str>>>,
        /// The versions a switch was asked for.
        switched: Arc<Mutex<Vec<String>>>,
        engine: Engine,
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
        let downloads = Arc::new(super::super::download::tests::Fake::default());
        let busy = Arc::new(Mutex::new(None));
        let switched = Arc::new(Mutex::new(Vec::new()));
        let (b, sw, verify_keys) = (busy.clone(), switched.clone(), trust.clone());
        let installer = Installer {
            source: downloads.clone(),
            busy: Box::new(move || *lock(&b)),
            switch: Box::new(move |v| lock(&sw).push(v.to_string())),
            target: Some(lodestar_update::Target::X86_64LinuxMusl),
            // The supervisor's verification, with the test keys.
            verify: Box::new(move |bin, v| {
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
        };
        let engine = Engine::with(
            Context {
                data_dir: data.unwrap_or_else(|| dir.path().to_path_buf()),
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
            Box::new(move || time.load(std::sync::atomic::Ordering::Relaxed)),
            Box::new(move || Ok(keys.embedded(vec![]))),
            installer,
        );
        Env {
            dir,
            fake,
            trust,
            settings,
            events,
            clock,
            downloads,
            busy,
            switched,
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

        let s = e.engine.pause(7);
        assert_eq!(s.policy.paused_until, Some(NOW + 7 * 86_400));
        assert_eq!(e.engine.pause(0).policy.paused_until, None);

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
