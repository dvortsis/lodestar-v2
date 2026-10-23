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
use super::layout::Bin;
use super::log_events::{UPDATE_AVAILABLE, UPDATE_CHECK_FAILED};
use super::policy::{self, Block, Reason};
use super::window::{When, Window};
use super::{history, pending, trust_file};
use crate::settings::updates::{Mode, Updates};

/// Between scheduled checks, before the random minute.
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Between manual checks.
const MANUAL_EVERY: Duration = Duration::from_secs(60);
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
    /// `updater.json` as the engine knows it: the source of truth, which
    /// the file only keeps across restarts. Were the file the truth, a
    /// `<data>/bin` that can't be written would leave no next check, and
    /// the schedule would check back to back.
    updater: Mutex<Updater>,
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
            Box::new(unix_now),
            Box::new(super::trust_root),
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
                    () = sleep(wait) => {}
                }
                if engine.scheduled_on() && engine.until_scheduled().is_some_and(|d| d.is_zero()) {
                    let _ = engine.run_check().await;
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
        Some(Duration::from_secs(next.saturating_sub(now).max(0) as u64))
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
                if let Some(v) = newest {
                    log_event!(UPDATE_AVAILABLE);
                    self.publish(UpdatesState::Available, Some(v), None);
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
            let marks: Vec<_> = bin
                .overlays()
                .unwrap_or_default()
                .iter()
                .filter_map(|v| super::overlay::verify(&bin, v).ok())
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

    fn skipped(&self) -> Vec<String> {
        #[derive(Deserialize)]
        struct Skip {
            version: String,
        }
        self.inner
            .bin
            .as_ref()
            .and_then(|b| b.read::<Vec<Skip>>("skipped").ok().flatten())
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.version)
            .collect()
    }

    /// Why this installation can't install itself (part 4 installs
    /// nothing, so `not_built_yet` when nothing else stops it).
    fn incapable(&self, settings: &Updates, trust_damaged: bool) -> Reason {
        let ctx = &self.inner.ctx;
        if !ctx.supervised {
            return Reason::NotSupervised;
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
        if noexec(bin.dir()) {
            return Reason::Noexec;
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
        let incapable = Some(self.incapable(settings, found.trust_damaged));
        policy::plan(&policy::Inputs {
            settings,
            releases: &c.manifest.releases,
            current: c.current,
            running: &self.inner.ctx.running,
            skipped: &skipped,
            paused_until: self.updater().paused_until,
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
        let updater = self.updater();
        let plan = self.plan(&settings, now);
        let found = lock(&self.inner.found);
        let reason = self.incapable(&settings, found.trust_damaged);
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
        if found.checked.as_ref().is_some_and(|c| !c.current) {
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
        Ok(self.status())
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
            .ok_or_else(|| UpdatesError::Internal("no bin".into()))?;
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

/// Whether `dir` is on a `noexec` mount. rustix has `ST_NOEXEC` only on
/// Linux, the one shipped Unix target; elsewhere (macOS, a developer's
/// machine) it isn't checked.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn noexec(dir: &std::path::Path) -> bool {
    rustix::fs::statvfs(dir).is_ok_and(|s| s.f_flag.contains(rustix::fs::StatVfsMountFlags::NOEXEC))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn noexec(_dir: &std::path::Path) -> bool {
    false
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
    fn status(&self) -> BoxFuture<'_, UpdatesStatus> {
        Box::pin(async move { Engine::status(self) })
    }

    fn check(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(self.check_now())
    }

    fn skip(&self, version: String) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(async move { Engine::skip(self, &version) })
    }

    fn pause(&self, days: u32) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(async move { Ok(Engine::pause(self, days)) })
    }

    fn clear_history(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(async move { Engine::clear_history(self) })
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
            Box::new(|| NOW),
            Box::new(move || Ok(keys.embedded(vec![]))),
        );
        Env {
            dir,
            fake,
            trust,
            settings,
            events,
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
        assert_eq!(e.states(), [UpdatesState::Checking]);
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
