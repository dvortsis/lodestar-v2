//! The update engine (§12.2) in `serve --child`: the scheduled and manual
//! signed checks, `updater.json`, the policy over what a check found, and
//! everything `/updates` and `/system` report. Part 3 of B-update checks,
//! decides and reports; installing comes in part 4, so `can_install` is
//! `not_built_yet` wherever nothing else stops it.
//!
//! - **When:** with `mode` notify, auto or image, at most every 6 hours
//!   at a random minute (`updater.json` keeps the last and next check);
//!   `POST /updates/check` at any mode, at most once a minute, one at a
//!   time.
//! - **The check:** [`check::fetch`], then `lodestar_update::check`
//!   against `trust.json` (rebuilt from the floor and the overlays when
//!   missing; a damaged one blocks installs and is never written), then
//!   the forward-only write of the next trust state.
//! - **Events:** `checking` when a check starts, `available` when it
//!   found a candidate, `failed` with its code.

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
    hub: Option<Arc<EventHub>>,
    fetch: Arc<dyn Fetch>,
    zone: (jiff::tz::TimeZone, String),
    found: Mutex<Found>,
    /// One check at a time.
    checking: tokio::sync::Mutex<()>,
    last_manual: Mutex<Option<Instant>>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

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
        hub: Option<Arc<EventHub>>,
        fetch: Arc<dyn Fetch>,
    ) -> Engine {
        let (tz, name, _) = super::window::local_zone();
        Engine::with(ctx, settings, hub, fetch, (tz, name), Box::new(unix_now))
    }

    fn with(
        ctx: Context,
        settings: watch::Receiver<Updates>,
        hub: Option<Arc<EventHub>>,
        fetch: Arc<dyn Fetch>,
        zone: (jiff::tz::TimeZone, String),
        clock: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Engine {
        let bin = Bin::open(&ctx.data_dir).ok();
        Engine {
            inner: Arc::new(Inner {
                ctx,
                bin,
                settings,
                hub,
                fetch,
                zone,
                found: Mutex::new(Found::default()),
                checking: tokio::sync::Mutex::new(()),
                last_manual: Mutex::new(None),
                clock,
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
        self.inner
            .bin
            .as_ref()
            .and_then(|b| b.read::<Updater>(UPDATER_FILE).ok().flatten())
            .unwrap_or_default()
    }

    fn save_updater(&self, f: impl FnOnce(&mut Updater)) {
        if let Some(bin) = &self.inner.bin {
            let mut u = self.updater();
            u.format = 1;
            f(&mut u);
            let _ = bin.write(UPDATER_FILE, &u);
        }
    }

    fn publish(&self, state: UpdatesState, version: Option<String>, error: Option<String>) {
        if let Some(hub) = &self.inner.hub {
            hub.publish(&Event::Updates(UpdatesEvent {
                state,
                version,
                bytes_done: None,
                bytes_total: None,
                error,
            }));
        }
    }

    /// One check: fetch, verify, advance `trust.json`, keep the result.
    async fn run_check(&self) -> Result<(), &'static str> {
        let _one = self.inner.checking.lock().await;
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
        let embedded = super::trust_root().map_err(lodestar_update::Error::code)?;
        let Some(bin) = self.inner.bin.clone() else {
            return Err("bin_unavailable");
        };
        let now = (self.inner.clock)();
        let checked = tokio::task::spawn_blocking(move || {
            let mut loaded = trust_file::load(bin.dir(), &embedded, &[]);
            if matches!(loaded, trust_file::Loaded::Rebuilt(_)) {
                // The overlays' marks matter only for a rebuild.
                let marks: Vec<_> = bin
                    .overlays()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|v| super::overlay::verify(&bin, v).ok())
                    .map(|v| v.overlay.mark)
                    .collect();
                loaded = trust_file::load(bin.dir(), &embedded, &marks);
            }
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

    /// Why this installation can't install itself (part 3 installs
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
        if rustix::fs::statvfs(bin.dir())
            .is_ok_and(|s| s.f_flag.contains(rustix::fs::StatVfsMountFlags::NOEXEC))
        {
            return Reason::Noexec;
        }
        if rustix::fs::access(bin.dir(), rustix::fs::Access::WRITE_OK).is_err() {
            return Reason::NotWritable;
        }
        if trust_damaged {
            return Reason::TrustDamaged;
        }
        let keys = super::trust_root().ok();
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

    /// Roll back arrives with installing (part 4); until then it's never
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
        {
            let mut last = lock(&self.inner.last_manual);
            if let Some(at) = *last {
                let since = at.elapsed();
                if since < MANUAL_EVERY {
                    return Err(UpdatesError::RateLimited {
                        retry_after: (MANUAL_EVERY - since).as_secs().max(1),
                    });
                }
            }
            if self.inner.checking.try_lock().is_err() {
                return Err(UpdatesError::RateLimited { retry_after: 5 });
            }
            *last = Some(Instant::now());
        }
        let _ = self.run_check().await;
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
