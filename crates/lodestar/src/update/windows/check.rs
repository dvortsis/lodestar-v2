//! Check and notify on Windows (§12.2, PK-win-upd-1), in the service or
//! the portable build: what the Unix engine does before it installs.
//!
//! - **The same pieces, unchanged:** [`update::window`](crate::update::window),
//!   [`update::policy`](crate::update::policy), the signed check's fetch
//!   and request ([`update::check`](crate::update::check)), `trust.json`
//!   ([`trust_file`]) and `lodestar_update::check`. There are no overlays
//!   on Windows, so the trust floor is the embedded statements alone.
//! - **When:** with `mode` notify or auto, every 6 hours plus a random
//!   minute within the hour; `POST /updates/check` at any mode, at most
//!   once a minute and one at a time. `<data>\bin\updater.json` keeps the
//!   times and the pause, as on Unix.
//! - **What's offered:** releases with an artifact for this installation
//!   ([`offered`]): the MSI for the service, the portable zip for the
//!   portable build, either when the kind isn't known ([`detect`]). The
//!   artifact an install would take is [`select`]'s, which never guesses:
//!   an unknown kind takes none.
//! - **Install** (the MSI, PK-win-upd-2): `POST /updates/install` with
//!   `when: now` downloads the MSI and hands it to the update helper
//!   ([`super::msi::hand_over`], [`super::helper`]). The portable build
//!   downloads its zip and installs it itself, then restarts into the new
//!   program ([`super::portable`]). Roll back comes with PK-win-upd-3
//!   (`not_built_yet`). Nothing installs automatically: the policy
//!   treats `auto` as Check and notify.
//! - **The one writer:** the notifier holds `<data>\bin\.lock` for its
//!   whole life. If another process holds it, nothing is fetched or
//!   written (`bin_unavailable`).
//! - **Untrusted input:** every file read from the data folder
//!   (`updater.json`, `skipped`, `trust.json`) is size-capped, parsed with
//!   unknown fields refused, opened without following a reparse point, and
//!   never trusted for more than the schedule and what's shown: the trust
//!   state is verified against the embedded keys and raised to their floor
//!   (a damaged one is never overwritten).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_api::dto::events::{Event, UpdatesEvent, UpdatesState};
use lodestar_api::events::EventHub;
use lodestar_api::ops::BoxFuture;
use lodestar_api::updates::{
    Blocked, Capability, CheckState, CheckStatus, InstallWhen, Policy, ReleaseInfo, RollbackInfo,
    Running, UpdatesApi, UpdatesError, UpdatesStatus,
};
use lodestar_log::log_event;
use lodestar_update::doc::{Artifact, ArtifactKind, Release, Target};
use lodestar_update::semver::Version;
use lodestar_update::{Checked, Embedded, Notice};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::settings::updates::{Level, Mode, Updates};
use crate::update::check::{self, CheckError, Fetch};
use crate::update::layout::{Bin, Lock, Step};
use crate::update::log_events::{
    UPDATE_AVAILABLE, UPDATE_CHECK_FAILED, UPDATE_INSTALL_FAILED, UPDATE_STATE_UNREADABLE,
    UPDATE_STATE_UNSAVED,
};
use crate::update::policy::{self, Block, Reason};
use crate::update::trust_file;
use crate::update::window::{When, Window};

/// Between scheduled checks, before the random minute.
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Between manual checks.
const MANUAL_EVERY: Duration = Duration::from_secs(60);
/// The longest the schedule sleeps before it looks at the clock again.
const MAX_SLEEP: Duration = Duration::from_secs(15 * 60);
pub const UPDATER_FILE: &str = "updater.json";
pub const SKIPPED_FILE: &str = "skipped";
/// The update helper's answer, and the notice, when Windows wouldn't start
/// the service at the end of an install (error 1920) and a retry remains.
pub const INSTALL_BLOCKED: &str = "install_blocked";
/// The notice for a blocked install whose retry is waiting while updates
/// are Off: switching Off and pausing can't take it back from there.
pub const INSTALL_BLOCKED_OFF: &str = "install_blocked_off";
/// Why Install and Roll back refuse until they're built on Windows (the
/// one place to change).
pub const NOT_BUILT_YET: Reason = Reason::NotBuiltYet;

/// How Lodestar was installed, which decides the artifact it updates
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// The MSI: the `Lodestar` service, from `%ProgramFiles%\Lodestar`.
    Msi,
    /// The portable zip, run from the folder it was unpacked to.
    Portable,
    /// Neither for certain: notified, never given an artifact.
    Unknown,
}

/// What [`detect`] looks at. Every field is a fact about this process;
/// nothing comes from the data folder.
#[derive(Debug, Clone, Default)]
pub struct Signals {
    /// The process runs as a service's virtual account (`NT SERVICE\…`,
    /// SID `S-1-5-80-…`), as the MSI's service does.
    pub service_account: bool,
    /// This executable (`current_exe`).
    pub exe: Option<PathBuf>,
    /// `%ProgramFiles%`.
    pub program_files: Option<PathBuf>,
    /// The data folder in use.
    pub data_dir: PathBuf,
    /// `Start Lodestar.cmd`, the portable zip's launcher, is a regular
    /// file beside the executable.
    pub launcher_beside_exe: bool,
}

/// The portable zip's launcher, beside `lodestar.exe`.
pub const LAUNCHER: &str = "Start Lodestar.cmd";

/// How Lodestar was installed, failing safe: each kind needs every one
/// of its signs, and anything else is [`InstallKind::Unknown`].
///
/// - **MSI:** a service account, running `%ProgramFiles%\Lodestar\lodestar.exe`
///   (the MSI installs only there: `main.wxs` `WrongInstallFolder`).
/// - **Portable:** not a service account, not under `%ProgramFiles%`,
///   the launcher beside the executable, and the data folder the
///   launcher sets (`<exe folder>\data`).
pub fn detect(s: &Signals) -> InstallKind {
    let Some(exe) = s.exe.as_deref().map(normal) else {
        return InstallKind::Unknown;
    };
    let program_files = s.program_files.as_deref().map(normal);
    let in_program_files = program_files
        .as_deref()
        .is_some_and(|pf| exe.starts_with(&format!("{pf}\\")));
    if s.service_account {
        let msi = program_files
            .as_deref()
            .is_some_and(|pf| exe == format!("{pf}\\lodestar\\lodestar.exe"));
        return if msi {
            InstallKind::Msi
        } else {
            InstallKind::Unknown
        };
    }
    let Some((folder, _)) = exe.rsplit_once('\\') else {
        return InstallKind::Unknown;
    };
    if !in_program_files
        && s.launcher_beside_exe
        && normal(&s.data_dir) == format!("{folder}\\data")
    {
        InstallKind::Portable
    } else {
        InstallKind::Unknown
    }
}

/// A Windows path compared as Windows does: case-insensitive, either
/// separator, no `\\?\` prefix, no trailing separator.
fn normal(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.trim_end_matches('\\').to_string()
}

/// The facts [`detect`] needs, read from the system.
#[cfg(windows)]
pub fn signals(data_dir: &Path) -> Signals {
    let exe = std::env::current_exe().ok();
    let launcher_beside_exe = exe
        .as_deref()
        .and_then(Path::parent)
        .and_then(|d| std::fs::symlink_metadata(d.join(LAUNCHER)).ok())
        .is_some_and(|m| m.is_file());
    Signals {
        service_account: crate::windows::acl::process_sid()
            .is_some_and(|sid| sid.starts_with("S-1-5-80-")),
        exe,
        program_files: super::msi::system_program_files(),
        data_dir: data_dir.to_path_buf(),
        launcher_beside_exe,
    }
}

/// Why [`select`] chose no artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoArtifact {
    /// This build's target isn't one the manifest ships.
    NoTarget,
    /// The install kind isn't known, so no artifact is chosen.
    UnknownKind,
    /// The release has no artifact of this kind for this target.
    Missing,
}

impl NoArtifact {
    pub fn code(self) -> &'static str {
        match self {
            NoArtifact::NoTarget => "no_target",
            NoArtifact::UnknownKind => "install_kind_unknown",
            NoArtifact::Missing => "no_artifact",
        }
    }
}

/// The artifact this installation would update from: the MSI for the
/// service, the portable zip for the portable build, for this target.
pub fn select(
    release: &Release,
    target: Option<Target>,
    kind: InstallKind,
) -> Result<&Artifact, NoArtifact> {
    let target = target.ok_or(NoArtifact::NoTarget)?;
    let want = match kind {
        InstallKind::Msi => ArtifactKind::Msi,
        InstallKind::Portable => ArtifactKind::Portable,
        InstallKind::Unknown => return Err(NoArtifact::UnknownKind),
    };
    release
        .artifacts
        .iter()
        .find(|a| a.target == target && a.kind == want)
        .ok_or(NoArtifact::Missing)
}

/// Whether a release is shown here: it has this installation's artifact,
/// or, when the kind isn't known, an MSI or portable zip for this target.
/// A release without one (a Linux-only fix) isn't for this machine.
pub fn offered(release: &Release, target: Option<Target>, kind: InstallKind) -> bool {
    match select(release, target, kind) {
        Ok(_) => true,
        Err(NoArtifact::UnknownKind) => release.artifacts.iter().any(|a| {
            Some(a.target) == target && matches!(a.kind, ArtifactKind::Msi | ArtifactKind::Portable)
        }),
        Err(_) => false,
    }
}

/// `updater.json`: the fields the Unix engine writes, so either reads the
/// other's. `queued` and `rolled_back_seen` are kept as found and never
/// set here.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolled_back_seen: Option<u64>,
}

/// One `skipped` entry, as the Unix engine writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Skip {
    version: String,
    reason: String,
}

/// The most `skipped` keeps: the oldest `user` skips go first, and a
/// `failed` or `manual` entry is never dropped ([`trim`]). Unix's
/// `pending::trim` drops the oldest of any reason once only those are
/// left; here they stay, so a version that failed stays refused.
const MAX_SKIPPED: usize = 64;
/// A version whose install failed (written by the update helper): never
/// offered again.
pub const SKIP_FAILED: &str = "failed";

/// Whether an entry is one the user can't undo (`failed`, `manual`).
fn refusing(s: &Skip) -> bool {
    s.reason == SKIP_FAILED || s.reason == "manual"
}

/// Keeps at most [`MAX_SKIPPED`] by dropping the oldest `user` skips,
/// never the entry just added (the last), never a `failed` or `manual`
/// one.
fn trim(list: &mut Vec<Skip>) {
    let mut over = list.len().saturating_sub(MAX_SKIPPED);
    let newest = list.len().saturating_sub(1);
    let mut i = 0;
    list.retain(|s| {
        let drop = over > 0 && i < newest && !refusing(s);
        over -= usize::from(drop);
        i += 1;
        !drop
    });
}

/// Adds `version` to `skipped` with `reason` (the update helper's
/// `failed`), under the caller's `.lock`. An entry that refuses it
/// already is kept; a `user` one is replaced. An unreadable list is an
/// error, never overwritten.
pub fn add_skip(bin: &Bin, version: &str, reason: &str) -> std::io::Result<()> {
    let mut list: Vec<Skip> = bin.read(SKIPPED_FILE)?.unwrap_or_default();
    if list
        .iter()
        .any(|s| s.version == version && (s.reason == reason || refusing(s)))
    {
        return Ok(());
    }
    list.retain(|s| s.version != version);
    list.push(Skip {
        version: version.to_string(),
        reason: reason.to_string(),
    });
    trim(&mut list);
    bin.write(SKIPPED_FILE, &list)
}

/// What the notifier knows about this process.
#[derive(Debug, Clone)]
pub struct Context {
    pub data_dir: PathBuf,
    pub running: Version,
    /// `CARGO_PKG_REPOSITORY`.
    pub repository: String,
    /// A development build: it has no release version to update from.
    pub dev: bool,
    pub kind: InstallKind,
    /// This build's target; `None` where nothing is shipped for it.
    pub target: Option<Target>,
    /// The data folder is the service's default, which the service checks
    /// at every start: the only one the update helper installs from.
    pub checked_data: bool,
    /// The store (`Config::db_path`): the portable rollback's snapshots
    /// are beside it.
    pub db: PathBuf,
}

/// The id of the request waiting for the update helper, if one is there
/// and readable (capped, parsed as the helper does).
fn pending_request(data_dir: &Path) -> Option<String> {
    let path = data_dir.join(super::msi::UPDATES).join(super::msi::REQUEST);
    let file = crate::update::layout::open_nofollow(&path).ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, super::msi::MAX_REQUEST + 1),
        &mut bytes,
    )
    .ok()?;
    super::msi::parse_request(&bytes).ok().map(|(r, _)| r.id)
}

/// Aborts the task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Where `updates` events go: the event hub, or a test's list.
pub type Publish = Arc<dyn Fn(UpdatesEvent) + Send + Sync>;
type Fault = Box<dyn Fn(Step) -> std::io::Result<()> + Send + Sync>;
type Keys = Box<dyn Fn() -> Result<Embedded, lodestar_update::Error> + Send + Sync>;

/// What tests replace: the clock, the keys, the writes' faults, and the
/// MSI's download and helper start (`None`: Install isn't offered).
pub struct Seams {
    pub clock: Box<dyn Fn() -> i64 + Send + Sync>,
    pub keys: Keys,
    pub fault: Fault,
    pub installer: Option<super::msi::Installer>,
    /// The portable build's own install (PK-win-portable-upd); `None`:
    /// not offered.
    pub portable: Option<Portable>,
}

/// The portable install's system side.
pub type Portable = Arc<dyn super::portable::Sys + Send + Sync>;

/// What the last check found.
#[derive(Default)]
struct Found {
    state: Option<CheckState>,
    checked: Option<Checked>,
    /// The envelope `checked` came from, as fetched: the MSI's helper
    /// verifies it again itself.
    envelope: Option<Vec<u8>>,
    notices: Vec<String>,
    trust_damaged: bool,
}

/// `updater.json` in this process.
#[derive(Debug, Default, Clone)]
struct State {
    /// As last written or read: every write starts from it.
    durable: Updater,
    /// The last check's times when they couldn't be written.
    unsaved: Option<(i64, i64, Option<String>)>,
    /// It was there but couldn't be read: only a pause or resume writes
    /// over it (a pause in it is unknown).
    unreadable: bool,
}

impl State {
    fn effective(&self) -> Updater {
        let mut u = self.durable.clone();
        if let Some((at, next, error)) = &self.unsaved {
            u.checked_at = Some(*at);
            u.next_check_at = Some(*next);
            u.error = error.clone();
        }
        u
    }
}

/// The notifier: cheap to clone.
#[derive(Clone)]
pub struct Notifier {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifier").finish_non_exhaustive()
    }
}

struct Inner {
    ctx: Context,
    /// `<data>\bin` and its held `.lock`; see [`Notifier::bin`].
    bin: Mutex<Option<(Bin, Lock)>>,
    settings: watch::Receiver<Updates>,
    publish: Publish,
    fetch: Arc<dyn Fetch>,
    zone: (jiff::tz::TimeZone, String),
    found: Mutex<Found>,
    /// Held through every write of `updater.json` or `skipped`.
    state: Mutex<State>,
    checking: Arc<tokio::sync::Mutex<()>>,
    last_manual: Mutex<Option<Instant>>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    keys: Keys,
    fault: Fault,
    installer: Option<super::msi::Installer>,
    portable: Option<Portable>,
    /// Bumped each time updates are switched Off or a pause starts
    /// ([`Notifier::withdraw`]): an install that wrote its request across
    /// a bump takes the request back.
    withdrawals: std::sync::atomic::AtomicU64,
    /// Settings changes the Off watcher has handled, for the tests.
    #[cfg(test)]
    handled: std::sync::atomic::AtomicU64,
    /// The version being downloaded for the helper.
    installing: Mutex<Option<Version>>,
}

impl Notifier {
    pub fn new(
        ctx: Context,
        settings: watch::Receiver<Updates>,
        hub: Arc<EventHub>,
        fetch: Arc<dyn Fetch>,
    ) -> Notifier {
        let (tz, name, _) = crate::update::window::local_zone();
        let publish: Publish = Arc::new(move |e| {
            hub.publish(&Event::Updates(e));
        });
        Notifier::with(
            ctx,
            settings,
            publish,
            fetch,
            (tz, name),
            Seams {
                clock: Box::new(crate::unix_now),
                keys: Box::new(Embedded::builtin),
                fault: Box::new(|_| Ok(())),
                installer: installer(),
                portable: portable(),
            },
        )
    }

    pub fn with(
        ctx: Context,
        settings: watch::Receiver<Updates>,
        publish: Publish,
        fetch: Arc<dyn Fetch>,
        zone: (jiff::tz::TimeZone, String),
        Seams {
            clock,
            keys,
            fault,
            installer,
            portable,
        }: Seams,
    ) -> Notifier {
        let n = Notifier {
            inner: Arc::new(Inner {
                ctx,
                bin: Mutex::new(None),
                settings,
                publish,
                fetch,
                zone,
                found: Mutex::new(Found::default()),
                state: Mutex::new(State::default()),
                checking: Arc::new(tokio::sync::Mutex::new(())),
                last_manual: Mutex::new(None),
                clock,
                keys,
                fault,
                installer,
                portable,
                withdrawals: std::sync::atomic::AtomicU64::new(0),
                #[cfg(test)]
                handled: std::sync::atomic::AtomicU64::new(0),
                installing: Mutex::new(None),
            }),
        };
        let _ = n.bin();
        n
    }

    /// `<data>\bin` with its lock held: this process is its one writer.
    /// Opened again when it couldn't be (the lock was busy at start, as
    /// when a relaunched copy starts before the old one has let go), so
    /// one busy moment doesn't turn updates off for the whole run. Its
    /// `updater.json` is read when it's first held. Never called with the
    /// state's lock held.
    fn bin(&self) -> Option<Bin> {
        let mut held = lock(&self.inner.bin);
        if held.is_none() {
            let bin = Bin::open(&self.inner.ctx.data_dir).ok()?;
            let Ok(Some(l)) = bin.lock() else {
                return None;
            };
            let mut state = lock(&self.inner.state);
            match bin.read::<Updater>(UPDATER_FILE) {
                Ok(u) => {
                    state.durable = u.unwrap_or_default();
                    state.unreadable = false;
                }
                Err(e) => {
                    log_event!(UPDATE_STATE_UNREADABLE, error = lodestar_log::Error::io(&e));
                    state.unreadable = true;
                }
            }
            *held = Some((bin, l));
        }
        held.as_ref().map(|(b, _)| b.clone())
    }

    /// The scheduled checks, until `stop` is set.
    pub fn spawn(&self, mut stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        let n = self.clone();
        let mut settings = self.inner.settings.clone();
        // A switch to Off is acted on at once, by a task of its own, never
        // held up by a check or an install holding the check lock. Only
        // the switch counts: an Install now made while updates are already
        // Off goes ahead, and keeps its retry across a restart.
        let watcher = {
            let n = self.clone();
            let mut settings = self.inner.settings.clone();
            let mut stop = stop.clone();
            // Read now, not when the task first runs: a switch made in
            // between would otherwise be taken for the starting mode.
            let mut was_off = settings.borrow_and_update().mode == Mode::Off;
            tokio::spawn(async move {
                let handle = |n: Notifier, before: bool| {
                    blocking(move || {
                        let mut w = before;
                        n.settings_changed(&mut w);
                        #[cfg(test)]
                        n.inner
                            .handled
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        w
                    })
                };
                loop {
                    // A change waiting is handled before a stop, and once
                    // more at the stop, so a switch to Off just before the
                    // service stops still takes its request back.
                    tokio::select! {
                        biased;
                        r = settings.changed() => {
                            if r.is_err() { return; }
                            was_off = handle(n.clone(), was_off).await;
                        }
                        _ = stop.changed() => {
                            handle(n.clone(), was_off).await;
                            return;
                        }
                    }
                }
            })
        };
        tokio::spawn(async move {
            // Aborted if this task is; otherwise waited for. The service's
            // shutdown aborts this task right after the stop, so the
            // watcher's last look is best effort: a removal already on the
            // blocking pool finishes, one not yet started may not.
            let mut watcher = AbortOnDrop(watcher);
            let checks = async {
                loop {
                    let wait = n.until_scheduled();
                    tokio::select! {
                        _ = stop.changed() => return,
                        r = settings.changed() => {
                            if r.is_err() { return; }
                            continue;
                        }
                        () = sleep(wait.map(|w| w.min(MAX_SLEEP))) => {}
                    }
                    if n.due() {
                        let one = tokio::select! {
                            one = n.inner.checking.clone().lock_owned() => one,
                            _ = stop.changed() => return,
                        };
                        // Still due once the lock is ours: a manual check may
                        // have run meanwhile.
                        if n.due() {
                            n.run_check(one).await;
                        }
                    }
                }
            };
            checks.await;
            let _ = (&mut watcher.0).await;
        })
    }

    /// How long until the next scheduled check; `None` with updates off.
    fn until_scheduled(&self) -> Option<Duration> {
        if self.inner.settings.borrow().mode == Mode::Off {
            return None;
        }
        let now = (self.inner.clock)();
        let next = self.updater().next_check_at.unwrap_or(now);
        // Further ahead than one interval can be (a clock corrected
        // backwards): due now.
        if next > now.saturating_add(EVERY.as_secs() as i64 + 3600) {
            return Some(Duration::ZERO);
        }
        Some(Duration::from_secs(next.saturating_sub(now).max(0) as u64))
    }

    fn due(&self) -> bool {
        self.until_scheduled().is_some_and(|d| d.is_zero())
    }

    /// After a settings change: a switch to Off (not Off staying Off, nor
    /// any other setting) withdraws.
    fn settings_changed(&self, was_off: &mut bool) {
        let off = self.inner.settings.borrow().mode == Mode::Off;
        if off && !*was_off {
            self.withdraw();
        }
        *was_off = off;
    }

    /// Install now respects Off and a pause (owner §2.13; #677's flag):
    /// when updates are switched Off or a pause starts, the request
    /// waiting for the update helper goes, so the helper's boot retry
    /// can't replay it. The epoch is bumped first; an install writing its
    /// request meanwhile reads it after the write ([`Notifier::install_steps`]),
    /// so either it sees the bump or this removal comes after its write.
    /// One file's removal, so atomic. A request the helper has already
    /// taken is past recall.
    fn withdraw(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.inner.withdrawals.fetch_add(1, SeqCst);
        self.remove_request();
    }

    fn remove_request(&self) {
        let updates = self.inner.ctx.data_dir.join(super::msi::UPDATES);
        let Ok(_pin) = crate::update::layout::pin_dir(&updates) else {
            return;
        };
        let request = updates.join(super::msi::REQUEST);
        // Retried as the renames are, for antivirus or the search indexer
        // holding the file a moment: this one removal is the only guard.
        let mut removed = std::fs::remove_file(&request);
        for pause_ms in [5, 20, 80, 320, 640] {
            match &removed {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    std::thread::sleep(Duration::from_millis(pause_ms));
                    removed = std::fs::remove_file(&request);
                }
                _ => break,
            }
        }
        crate::update::layout::sync_dir(&updates);
        match removed {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                log_event!(
                    UPDATE_STATE_UNSAVED,
                    file = super::msi::REQUEST,
                    error = lodestar_log::Error::io(&e)
                );
            }
            _ => {}
        }
    }

    fn updater(&self) -> Updater {
        lock(&self.inner.state).effective()
    }

    /// For `/system`'s Updates row: whether updates are on, and when the
    /// last check completed.
    pub fn summary(&self) -> (bool, Option<i64>) {
        (
            self.inner.settings.borrow().mode != Mode::Off,
            self.updater().checked_at,
        )
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

    /// `POST /updates/check`: at most once a minute, one at a time.
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
            let Ok(one) = self.inner.checking.clone().try_lock_owned() else {
                return Err(UpdatesError::RateLimited { retry_after: 5 });
            };
            *last = Some(Instant::now());
            one
        };
        self.run_check(one).await;
        let n = self.clone();
        Ok(blocking(move || n.status()).await)
    }

    /// One check, holding the one-check lock: fetch, verify, advance
    /// `trust.json`, keep the result, save the times, publish the end.
    async fn run_check(&self, _one: tokio::sync::OwnedMutexGuard<()>) {
        let settings = self.inner.settings.borrow().clone();
        lock(&self.inner.found).state = Some(CheckState::Checking);
        self.publish(UpdatesState::Checking, None, None);
        let result = self.check_once(settings.channel.update()).await;
        let now = (self.inner.clock)();
        let next = now + EVERY.as_secs() as i64 + jitter();
        let error = result.err().map(str::to_string);
        let n = self.clone();
        blocking(move || n.save_check(now, next, error)).await;
        match result {
            Ok(()) => {
                lock(&self.inner.found).state = Some(CheckState::Ok);
                let n = self.clone();
                let newest = blocking(move || {
                    n.plan(n.bin().as_ref(), &settings, now)
                        .releases
                        .first()
                        .map(|a| a.version.to_string())
                })
                .await;
                match newest {
                    Some(v) => {
                        log_event!(UPDATE_AVAILABLE);
                        self.publish(UpdatesState::Available, Some(v), None);
                    }
                    None => self.publish(UpdatesState::Checked, None, None),
                }
            }
            Err(code) => {
                lock(&self.inner.found).state = Some(CheckState::Failed);
                log_event!(UPDATE_CHECK_FAILED, error = code);
                self.publish(UpdatesState::Failed, None, Some(code.to_string()));
            }
        }
    }

    async fn check_once(&self, channel: lodestar_update::Channel) -> Result<(), &'static str> {
        let Some(bin) = self.bin() else {
            return Err("bin_unavailable");
        };
        let url = check::source(&self.inner.ctx.repository, channel).map_err(CheckError::code)?;
        let Some(body) = check::fetch(&*self.inner.fetch, url)
            .await
            .map_err(CheckError::code)?
        else {
            // Nothing published for this channel yet.
            let mut f = lock(&self.inner.found);
            f.checked = None;
            f.envelope = None;
            f.notices.clear();
            f.trust_damaged = false;
            return Ok(());
        };
        let embedded = (self.inner.keys)().map_err(lodestar_update::Error::code)?;
        let now = (self.inner.clock)().max(0) as u64;
        let envelope = body.clone();
        let (checked, damaged) = blocking(move || {
            // No overlays on Windows: the floor is the embedded statements.
            let loaded = trust_file::load(bin.dir(), &embedded, &[]);
            let checked = lodestar_update::check(&body, channel, now, &embedded, loaded.state());
            if let Ok(c) = &checked {
                let _ = trust_file::save(bin.dir(), &loaded, c.next.clone());
            }
            (checked, loaded.damaged())
        })
        .await;
        let mut f = lock(&self.inner.found);
        f.trust_damaged = damaged;
        match checked {
            Ok(c) => {
                f.notices = c.notices.iter().map(|n| n.code().to_string()).collect();
                f.checked = Some(c);
                f.envelope = Some(envelope);
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

    /// Saves a check's times; kept in memory only when they can't be
    /// written, or when an unreadable file may hold a pause.
    fn save_check(&self, at: i64, next: i64, error: Option<String>) {
        let bin = self.bin();
        let mut state = lock(&self.inner.state);
        if !state.unreadable {
            let mut u = state.durable.clone();
            u.format = 1;
            (u.checked_at, u.next_check_at, u.error) = (Some(at), Some(next), error.clone());
            if self.write(bin.as_ref(), UPDATER_FILE, &u).is_ok() {
                state.durable = u;
                state.unsaved = None;
                return;
            }
        }
        state.unsaved = Some((at, next, error));
    }

    /// A durable write (acknowledged: a failed step is an error).
    fn write<T: Serialize>(
        &self,
        bin: Option<&Bin>,
        name: &'static str,
        value: &T,
    ) -> Result<(), UpdatesError> {
        let bin = bin.ok_or(UpdatesError::BinUnavailable)?;
        bin.write_acknowledged(name, value, &*self.inner.fault)
            .map_err(|e| {
                log_event!(
                    UPDATE_STATE_UNSAVED,
                    file = name,
                    error = lodestar_log::Error::io(&e)
                );
                UpdatesError::NotSaved
            })
    }

    /// The skipped versions; `None` when `skipped` can't be read.
    fn skipped(bin: Option<&Bin>) -> Option<Vec<Skip>> {
        let Some(bin) = bin else {
            return Some(vec![]);
        };
        Some(
            bin.read::<Vec<Skip>>(SKIPPED_FILE)
                .ok()?
                .unwrap_or_default(),
        )
    }

    /// Why this installation can't install itself automatically: always
    /// something here, since nothing installs on its own on Windows yet.
    fn incapable(&self, skipped_ok: bool) -> Reason {
        if !skipped_ok {
            Reason::SkippedUnreadable
        } else if self.inner.ctx.dev {
            Reason::DevBuild
        } else {
            NOT_BUILT_YET
        }
    }

    /// Why Install isn't offered, or `None` when it is: the MSI, through
    /// the update helper (PK-win-upd-2), from the checked data folder; the
    /// portable build through its own steps, with a private `<data>\bin`
    /// ([`super::portable::cannot_install`]).
    fn cannot_install(&self, skipped_ok: bool) -> Option<&'static str> {
        let ctx = &self.inner.ctx;
        match self.incapable(skipped_ok) {
            NOT_BUILT_YET => {}
            r => return Some(r.code()),
        }
        match ctx.kind {
            InstallKind::Msi => {}
            InstallKind::Unknown => return Some(NoArtifact::UnknownKind.code()),
            InstallKind::Portable => {
                let (Some(sys), Some(_)) = (&self.inner.portable, &self.inner.installer) else {
                    return Some(NOT_BUILT_YET.code());
                };
                let Some(bin) = self.bin() else {
                    return Some("bin_unavailable");
                };
                return super::portable::cannot_install(&**sys, &ctx.data_dir, &bin);
            }
        }
        if !ctx.checked_data {
            return Some("data_folder_unchecked");
        }
        if self.inner.installer.is_none() {
            return Some(NOT_BUILT_YET.code());
        }
        None
    }

    /// The policy's plan over the releases offered here.
    fn plan(&self, bin: Option<&Bin>, settings: &Updates, now: i64) -> policy::Plan {
        let paused_until = self.updater().paused_until;
        let found = lock(&self.inner.found);
        let Some(c) = found.checked.as_ref() else {
            return policy::Plan::default();
        };
        let ctx = &self.inner.ctx;
        let releases: Vec<Release> = c
            .manifest
            .releases
            .iter()
            .filter(|r| offered(r, ctx.target, ctx.kind))
            .cloned()
            .collect();
        let window = Window::parse(&settings.window).ok();
        let tz = self.inner.zone.0.clone();
        let window_from = move |t: i64| match window.map(|w| w.when(t, &tz)) {
            Some(When::Open { .. }) | None => t,
            Some(When::Opens(at)) => at,
        };
        let skipped = Notifier::skipped(bin);
        let incapable = self.incapable(skipped.is_some());
        let skipped: Vec<String> = skipped
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.version)
            .collect();
        policy::plan(&policy::Inputs {
            settings,
            releases: &releases,
            current: current(c, now),
            running: &ctx.running,
            skipped: &skipped,
            paused_until,
            held_in_process: false,
            now,
            window_from: &window_from,
            incapable: Some(incapable),
        })
    }

    /// Everything `GET /updates` reports.
    pub fn status(&self) -> UpdatesStatus {
        let settings = self.inner.settings.borrow().clone();
        let now = (self.inner.clock)();
        let ctx = &self.inner.ctx;
        let bin = self.bin();
        let (updater, check_unsaved, unreadable) = {
            let s = lock(&self.inner.state);
            (s.effective(), s.unsaved.is_some(), s.unreadable)
        };
        let plan = self.plan(bin.as_ref(), &settings, now);
        let cannot = self.cannot_install(Notifier::skipped(bin.as_ref()).is_some());
        let installing = lock(&self.inner.installing).clone();
        let found = lock(&self.inner.found);
        let mut notices = found.notices.clone();
        if found.trust_damaged {
            notices.push(Notice::TrustDamaged.code().to_string());
        }
        if found.checked.as_ref().is_some_and(|c| !current(c, now)) {
            notices.push("manifest_lapsed".into());
        }
        if check_unsaved && bin.is_some() {
            notices.push("check_not_saved".into());
        }
        if unreadable {
            notices.push("updater_unreadable".into());
        }
        // The update helper's last answer, when it didn't install a newer
        // version (display only: this account can write that file too).
        // A blocked install (Windows wouldn't start the service; nothing
        // changed, and the helper tries again) has its own words.
        let missed = super::msi::answer(&ctx.data_dir).filter(|a| {
            a.outcome != "installed"
                && crate::update::layout::parse_version(&a.version).is_some_and(|v| v > ctx.running)
        });
        if let Some(a) = missed {
            // "Tries again" is true only while the request the helper will
            // retry is still there (Off or a pause takes it back).
            let retry =
                a.outcome == INSTALL_BLOCKED && pending_request(&ctx.data_dir) == Some(a.id);
            notices.push(if retry && settings.mode == Mode::Off {
                // Already Off: the way to cancel is a switch to Off.
                INSTALL_BLOCKED_OFF.into()
            } else if retry {
                INSTALL_BLOCKED.into()
            } else {
                "install_failed".into()
            });
        }
        notices.sort();
        notices.dedup();
        let releases = found
            .checked
            .as_ref()
            .map(|c| release_infos(&plan, &c.manifest.releases))
            .unwrap_or_default();
        let withdrawn = found.checked.as_ref().is_some_and(|c| {
            c.manifest
                .releases
                .iter()
                .any(|r| r.version == ctx.running && r.withdrawn)
        });
        let next_window_at =
            Window::parse(&settings.window)
                .ok()
                .map(|w| match w.when(now, &self.inner.zone.0) {
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
                channel: settings.channel.update().name().into(),
                image_version: None,
                overlay: false,
                revoked_key: false,
                withdrawn,
            },
            capability: Capability {
                can_install: cannot.is_none(),
                reason: cannot.map(str::to_string),
            },
            policy: Policy {
                mode: mode_name(settings.mode).into(),
                level: level_name(settings.level).into(),
                security_fixes: settings.security_effective(),
                wait_days: settings.wait_days,
                window: settings.window.clone(),
                window_zone: self.inner.zone.1.clone(),
                next_window_at,
                paused_until: updater.paused_until.filter(|&p| p > now),
                held_in_process: false,
                allow_reindex: settings.allow_reindex,
            },
            releases,
            pending: None,
            installing: installing.map(|v| lodestar_api::updates::Installing {
                version: v.to_string(),
                when: InstallWhen::Now,
            }),
            rollback: RollbackInfo {
                available: false,
                reason: Some(NOT_BUILT_YET.code().into()),
                to_version: None,
                lost_torrents: None,
                restores_store: None,
            },
            notices,
            history: vec![],
        }
    }

    /// `POST /updates/install` (the MSI): checks Install now's rules as
    /// the engine does, then downloads the MSI and hands it to the update
    /// helper in the background, answering at once. Only `when: now`:
    /// nothing installs at a window on Windows yet.
    pub async fn install_request(
        &self,
        version: String,
        when: InstallWhen,
        confirm_manual: bool,
    ) -> Result<UpdatesStatus, UpdatesError> {
        let v =
            crate::update::layout::parse_version(&version).ok_or(UpdatesError::UnknownVersion)?;
        // Read when Install now is asked for: a switch to Off from here on
        // takes the request back.
        let epoch = self
            .inner
            .withdrawals
            .load(std::sync::atomic::Ordering::SeqCst);
        let n = self.clone();
        let cannot = blocking(move || {
            let bin = n.bin();
            n.cannot_install(Notifier::skipped(bin.as_ref()).is_some())
        })
        .await;
        if let Some(reason) = cannot {
            return Err(UpdatesError::InstallUnavailable(reason));
        }
        if when == InstallWhen::Window {
            return Err(UpdatesError::InstallUnavailable(NOT_BUILT_YET.code()));
        }
        // One at a time, and never during a check.
        let Ok(one) = self.inner.checking.clone().try_lock_owned() else {
            return Err(UpdatesError::InstallUnavailable("busy"));
        };
        let (n, w) = (self.clone(), v.clone());
        let (pin, envelope, channel) = blocking(move || n.may_install(&w, confirm_manual)).await?;
        *lock(&self.inner.installing) = Some(v.clone());
        let n = self.clone();
        tokio::spawn(async move {
            n.run_install(one, v, pin, envelope, channel, epoch).await;
        });
        let n = self.clone();
        Ok(blocking(move || n.status()).await)
    }

    /// Install now's rules (the policy's, with nothing incapable: the
    /// MSI can install), the manual steps' confirmation, and the pin from
    /// a current manifest.
    fn may_install(
        &self,
        v: &Version,
        confirm_manual: bool,
    ) -> Result<(lodestar_update::Pin, Vec<u8>, lodestar_update::Channel), UpdatesError> {
        let settings = self.inner.settings.borrow().clone();
        let now = (self.inner.clock)();
        let bin = self.bin();
        let skipped: Vec<String> = Notifier::skipped(bin.as_ref())
            .ok_or(UpdatesError::InstallUnavailable(
                Reason::SkippedUnreadable.code(),
            ))?
            .into_iter()
            .map(|s| s.version)
            .collect();
        let paused_until = self.updater().paused_until;
        let ctx = &self.inner.ctx;
        let found = lock(&self.inner.found);
        let (Some(c), Some(envelope)) = (found.checked.as_ref(), found.envelope.as_ref()) else {
            return Err(UpdatesError::UnknownVersion);
        };
        let releases: Vec<Release> = c
            .manifest
            .releases
            .iter()
            .filter(|r| offered(r, ctx.target, ctx.kind))
            .cloned()
            .collect();
        let window_from = |t: i64| t;
        let inputs = policy::Inputs {
            settings: &settings,
            releases: &releases,
            current: current(c, now),
            running: &ctx.running,
            skipped: &skipped,
            paused_until,
            held_in_process: false,
            now,
            window_from: &window_from,
            incapable: None,
        };
        policy::install_now(&inputs, v).map_err(|r| match r {
            policy::Refusal::UnknownVersion => UpdatesError::UnknownVersion,
            policy::Refusal::Skipped => UpdatesError::VersionSkipped,
            policy::Refusal::NotCandidate => UpdatesError::InstallUnavailable("not_candidate"),
            policy::Refusal::Incapable(reason) => UpdatesError::InstallUnavailable(reason.code()),
            policy::Refusal::NotCurrent => UpdatesError::InstallUnavailable("not_current"),
            policy::Refusal::UpgradePath => UpdatesError::InstallUnavailable("upgrade_path"),
        })?;
        let manual = releases.iter().any(|r| r.version == *v && r.manual_steps);
        if manual && !confirm_manual {
            return Err(UpdatesError::ManualConfirmationRequired);
        }
        let target = ctx.target.ok_or(UpdatesError::InstallUnavailable(
            NoArtifact::NoTarget.code(),
        ))?;
        let kind = match ctx.kind {
            InstallKind::Portable => ArtifactKind::Portable,
            _ => ArtifactKind::Msi,
        };
        let pin = lodestar_update::install(c, v, target, kind)
            .map_err(|e| UpdatesError::InstallUnavailable(e.code()))?;
        Ok((pin, envelope.clone(), c.manifest.channel))
    }

    /// The background half of an install, holding the one-check lock: its
    /// events, and on failure the log line and the `failed` event.
    async fn run_install(
        &self,
        _one: tokio::sync::OwnedMutexGuard<()>,
        v: Version,
        pin: lodestar_update::Pin,
        envelope: Vec<u8>,
        channel: lodestar_update::Channel,
        epoch: u64,
    ) {
        let result = self.install_steps(&v, &pin, envelope, channel, epoch).await;
        *lock(&self.inner.installing) = None;
        match result {
            // The helper stops this service: what follows is the MSI's.
            Ok(()) => self.publish(UpdatesState::Installing, Some(v.to_string()), None),
            Err(code) => {
                log_event!(UPDATE_INSTALL_FAILED, error = code);
                self.publish(
                    UpdatesState::Failed,
                    Some(v.to_string()),
                    Some(code.to_string()),
                );
            }
        }
    }

    /// Downloads the MSI to `<data>\bin` (verified against the pin), hands
    /// it to the helper in `<data>\updates\` and starts the helper's task.
    /// The service has no other rights: the helper trusts none of this.
    async fn install_steps(
        &self,
        v: &Version,
        pin: &lodestar_update::Pin,
        envelope: Vec<u8>,
        channel: lodestar_update::Channel,
        epoch: u64,
    ) -> Result<(), &'static str> {
        let installer = self.inner.installer.as_ref().ok_or(NOT_BUILT_YET.code())?;
        let bin = self.bin().ok_or("bin_unavailable")?;
        (self.inner.publish)(UpdatesEvent {
            state: UpdatesState::Downloading,
            version: Some(v.to_string()),
            bytes_done: Some(0),
            bytes_total: Some(pin.size),
            error: None,
        });
        let part = crate::update::download::download(&*installer.source, &bin, pin, &mut |_| {})
            .await
            .map_err(crate::update::download::DownloadError::code)?;
        if let (InstallKind::Portable, Some(sys)) = (self.inner.ctx.kind, &self.inner.portable) {
            // The new program in place, then this run restarts into it
            // (Restart now's relaunch), or the old one goes back.
            let (sys, data, pin) = (sys.clone(), self.inner.ctx.data_dir.clone(), pin.clone());
            let db = self.inner.ctx.db.clone();
            return blocking(move || {
                super::portable::install(&*sys, &data, &db, &bin, &part, &pin)?;
                sys.restart().map_err(|_| {
                    super::portable::abandon(&*sys, &data);
                    "restart_unavailable"
                })
            })
            .await;
        }
        let (data, w) = (self.inner.ctx.data_dir.clone(), v.clone());
        blocking(move || super::msi::hand_over(&data, &part, &envelope, &w, channel))
            .await
            .map_err(|e| {
                log_event!(
                    UPDATE_STATE_UNSAVED,
                    file = super::msi::REQUEST,
                    error = lodestar_log::Error::io(&e)
                );
                "handover_failed"
            })?;
        // Updates went Off or a pause started while this install ran: the
        // request is taken back rather than left for the helper.
        if self
            .inner
            .withdrawals
            .load(std::sync::atomic::Ordering::SeqCst)
            != epoch
        {
            let n = self.clone();
            blocking(move || n.remove_request()).await;
            return Err("withdrawn");
        }
        (installer.start)().map_err(|_| "helper_unavailable")
    }

    /// `POST /updates/skip`: hides a version a check found, as on Unix
    /// (`skipped`, reason `user`); acknowledged only once it's durable.
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
        let bin = self.bin().ok_or(UpdatesError::BinUnavailable)?;
        {
            let _state = lock(&self.inner.state);
            // An unreadable list is damage: nothing is written over it.
            let mut list = Notifier::skipped(Some(&bin))
                .ok_or_else(|| UpdatesError::Internal("skipped is unreadable".into()))?;
            if !list.iter().any(|s| s.version == version) {
                list.push(Skip {
                    version: version.to_string(),
                    reason: "user".into(),
                });
                trim(&mut list);
                self.write(Some(&bin), SKIPPED_FILE, &list)?;
            }
        }
        Ok(self.status())
    }

    /// `POST /updates/pause`: `days` 0 resumes. Acknowledged only once
    /// it's durable; a saved one replaces an unreadable file. Nothing
    /// installs automatically here, so a failed pause holds nothing.
    pub fn pause(&self, days: u32) -> Result<UpdatesStatus, UpdatesError> {
        let now = (self.inner.clock)();
        let bin = self.bin();
        {
            let mut state = lock(&self.inner.state);
            let mut u = state.durable.clone();
            u.format = 1;
            u.paused_until = (days > 0).then(|| now + i64::from(days) * 86_400);
            self.write(bin.as_ref(), UPDATER_FILE, &u)?;
            state.durable = u;
            state.unreadable = false;
        }
        if days > 0 {
            self.withdraw();
        }
        Ok(self.status())
    }
}

/// The MSI's download and helper start, on Windows.
#[cfg(windows)]
fn installer() -> Option<super::msi::Installer> {
    super::msi::Installer::system()
}

#[cfg(not(windows))]
fn installer() -> Option<super::msi::Installer> {
    None
}

/// The portable build's install on Windows (used only for that kind).
#[cfg(windows)]
fn portable() -> Option<Portable> {
    Some(Arc::new(super::portable::System))
}

#[cfg(not(windows))]
fn portable() -> Option<Portable> {
    None
}

/// The releases as `/updates` lists them, in the policy's order. This
/// and the three below mirror `engine.rs`'s, which is Unix-only; a
/// follow-up moves them to a module both share.
fn release_infos(plan: &policy::Plan, releases: &[Release]) -> Vec<ReleaseInfo> {
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

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Fixes => "fixes",
        Level::Features => "features",
        Level::All => "all",
    }
}

/// Current when it was checked, and not expired since.
fn current(c: &Checked, now: i64) -> bool {
    c.current && (now.max(0) as u64) < c.expires
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

/// Runs `f` on the blocking pool (file I/O under `<data>\bin`).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl UpdatesApi for Notifier {
    fn status(&self) -> BoxFuture<'_, UpdatesStatus> {
        let n = self.clone();
        Box::pin(blocking(move || n.status()))
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
        Box::pin(self.install_request(version, when, confirm_manual))
    }

    fn skip(&self, version: String) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        let n = self.clone();
        Box::pin(blocking(move || n.skip(&version)))
    }

    fn pause(&self, days: u32) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        let n = self.clone();
        Box::pin(blocking(move || n.pause(days)))
    }

    /// There's no install history on Windows yet: nothing to clear.
    fn clear_history(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        let n = self.clone();
        Box::pin(async move { Ok(blocking(move || n.status()).await) })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(async { Err(UpdatesError::RollbackUnavailable(NOT_BUILT_YET.code())) })
    }
}

#[cfg(test)]
mod tests {
    use lodestar_update::Channel;
    use lodestar_update::testing::{TestTrust, bytes, envelope, manifest, release};
    use serde_json::{Value, json};

    use super::*;
    use crate::update::check::tests::Fake;

    const NOW: i64 = 1_800_000_000;
    const STABLE: &str =
        "https://github.com/dvortsis/lodestar-v2/releases/download/update-manifest/stable.json";
    const WIN: Option<Target> = Some(Target::X86_64WindowsMsvc);

    /// A release with the named artifacts: `msi`, `portable` (Windows) or
    /// `binary` (Linux).
    fn release_with(version: &str, kinds: &[&str]) -> Value {
        let mut r = release(version, NOW as u64, b"x");
        r["artifacts"] = kinds
            .iter()
            .map(|k| {
                let (target, file) = match *k {
                    "msi" => ("x86_64-pc-windows-msvc", "lodestar.msi"),
                    "portable" => ("x86_64-pc-windows-msvc", "lodestar.zip"),
                    _ => ("x86_64-unknown-linux-musl", "lodestar"),
                };
                json!({
                    "target": target,
                    "kind": k,
                    "url": format!(
                        "https://github.com/dvortsis/lodestar-v2/releases/download/v{version}/{file}"
                    ),
                    "sha256": lodestar_update::Sha256::of(b"x").to_hex(),
                    "size": 1,
                })
            })
            .collect();
        r
    }

    fn parsed(v: Value) -> Release {
        serde_json::from_value(v).unwrap()
    }

    // ---- How Lodestar was installed ----

    fn msi() -> Signals {
        Signals {
            service_account: true,
            exe: Some(r"C:\Program Files\Lodestar\lodestar.exe".into()),
            program_files: Some(r"C:\Program Files".into()),
            data_dir: r"C:\ProgramData\Lodestar".into(),
            launcher_beside_exe: false,
        }
    }

    fn portable() -> Signals {
        Signals {
            service_account: false,
            exe: Some(r"D:\Apps\Lodestar\lodestar.exe".into()),
            program_files: Some(r"C:\Program Files".into()),
            data_dir: r"D:\Apps\Lodestar\data".into(),
            launcher_beside_exe: true,
        }
    }

    #[test]
    fn the_msi_needs_the_service_account_and_the_install_folder() {
        assert_eq!(detect(&msi()), InstallKind::Msi);
        // Spelled differently, as Windows allows.
        let s = Signals {
            exe: Some(r"\\?\c:/PROGRAM FILES/lodestar/LODESTAR.EXE".into()),
            program_files: Some(r"C:\Program Files\".into()),
            ..msi()
        };
        assert_eq!(detect(&s), InstallKind::Msi);
        // A service account running a copy from anywhere else.
        let elsewhere = Signals {
            exe: Some(r"C:\Program Files\Other\lodestar.exe".into()),
            ..msi()
        };
        assert_eq!(detect(&elsewhere), InstallKind::Unknown);
        // The install folder, but not as a service.
        let by_hand = Signals {
            service_account: false,
            ..msi()
        };
        assert_eq!(detect(&by_hand), InstallKind::Unknown);
        // %ProgramFiles% unknown: not provably the MSI.
        let no_pf = Signals {
            program_files: None,
            ..msi()
        };
        assert_eq!(detect(&no_pf), InstallKind::Unknown);
    }

    #[test]
    fn portable_needs_the_launcher_and_its_data_folder() {
        assert_eq!(detect(&portable()), InstallKind::Portable);
        let lower = Signals {
            data_dir: r"d:/apps/lodestar/DATA/".into(),
            ..portable()
        };
        assert_eq!(detect(&lower), InstallKind::Portable);
        for s in [
            Signals {
                launcher_beside_exe: false,
                ..portable()
            },
            Signals {
                data_dir: r"D:\Elsewhere\data".into(),
                ..portable()
            },
            // Unpacked into Program Files: that's the MSI's place.
            Signals {
                exe: Some(r"C:\Program Files\Lodestar\lodestar.exe".into()),
                data_dir: r"C:\Program Files\Lodestar\data".into(),
                ..portable()
            },
            // A service account is never the portable build.
            Signals {
                service_account: true,
                ..portable()
            },
            Signals {
                exe: None,
                ..portable()
            },
            Signals::default(),
        ] {
            assert_eq!(detect(&s), InstallKind::Unknown, "{s:?}");
        }
    }

    // ---- Which artifact ----

    #[test]
    fn each_kind_takes_its_own_artifact() {
        let both = parsed(release_with("0.5.4", &["binary", "msi", "portable"]));
        let msi = select(&both, WIN, InstallKind::Msi).unwrap();
        assert_eq!(msi.kind, ArtifactKind::Msi);
        assert!(msi.url.ends_with("/lodestar.msi"));
        let zip = select(&both, WIN, InstallKind::Portable).unwrap();
        assert_eq!(zip.kind, ArtifactKind::Portable);
        assert!(zip.url.ends_with("/lodestar.zip"));
        // Unknown never guesses, but is still told about it.
        assert_eq!(
            select(&both, WIN, InstallKind::Unknown).unwrap_err(),
            NoArtifact::UnknownKind
        );
        for kind in [
            InstallKind::Msi,
            InstallKind::Portable,
            InstallKind::Unknown,
        ] {
            assert!(offered(&both, WIN, kind));
        }
    }

    #[test]
    fn a_missing_artifact_or_target_offers_nothing() {
        let msi_only = parsed(release_with("0.5.4", &["msi"]));
        assert_eq!(
            select(&msi_only, WIN, InstallKind::Portable).unwrap_err(),
            NoArtifact::Missing
        );
        assert!(!offered(&msi_only, WIN, InstallKind::Portable));
        assert!(offered(&msi_only, WIN, InstallKind::Unknown));
        // A Linux-only release isn't for this machine, whatever the kind.
        let linux = parsed(release_with("0.5.4", &["binary"]));
        for kind in [
            InstallKind::Msi,
            InstallKind::Portable,
            InstallKind::Unknown,
        ] {
            assert!(!offered(&linux, WIN, kind), "{kind:?}");
        }
        assert_eq!(
            select(&linux, WIN, InstallKind::Msi).unwrap_err(),
            NoArtifact::Missing
        );
        // A build for a target nothing ships (aarch64 Windows).
        let both = parsed(release_with("0.5.4", &["msi", "portable"]));
        assert_eq!(
            select(&both, None, InstallKind::Msi).unwrap_err(),
            NoArtifact::NoTarget
        );
        assert!(!offered(&both, None, InstallKind::Unknown));
        // The Linux target doesn't take the Windows artifacts.
        let musl = Some(Target::X86_64LinuxMusl);
        assert!(!offered(&both, musl, InstallKind::Msi));
        assert!(!offered(&both, musl, InstallKind::Unknown));
    }

    // ---- The notifier ----

    /// The notifier comes first, so it's dropped (and its folder
    /// released, which Windows needs) before the temporary folder.
    struct Env {
        n: Option<Notifier>,
        dir: tempfile::TempDir,
        fake: Arc<Fake>,
        trust: Arc<TestTrust>,
        settings: watch::Sender<Updates>,
        events: Arc<Mutex<Vec<UpdatesEvent>>>,
        /// The write step to fail, if any.
        fail: Arc<Mutex<Option<Step>>>,
        /// With the MSI's installer: its downloads, and how many times the
        /// helper's task was started.
        installer: bool,
        downloads: Arc<crate::update::download::tests::Fake>,
        started: Arc<std::sync::atomic::AtomicUsize>,
        /// The portable build's install, and its data folder when it
        /// isn't `dir`.
        portable: Option<Portable>,
        data: Option<PathBuf>,
        /// Runs `withdraw` on this notifier when the download opens: a
        /// switch to Off in the middle of an install.
        mid_download: Option<Arc<std::sync::OnceLock<Notifier>>>,
    }

    /// A download source that withdraws when it's opened (see
    /// `Env::mid_download`).
    struct Hooked {
        inner: Arc<crate::update::download::tests::Fake>,
        notifier: Arc<std::sync::OnceLock<Notifier>>,
    }

    impl crate::update::download::Source for Hooked {
        fn open<'a>(
            &'a self,
            url: &'a reqwest::Url,
        ) -> BoxFuture<'a, Result<crate::update::download::Opened, CheckError>> {
            if let Some(n) = self.notifier.get() {
                n.withdraw();
            }
            self.inner.open(url)
        }
    }

    impl Env {
        fn new(mode: Mode, kind: InstallKind) -> Env {
            let (settings, _) = watch::channel(Updates {
                mode,
                ..Updates::default()
            });
            let mut e = Env {
                dir: tempfile::tempdir().unwrap(),
                fake: Arc::new(Fake::default()),
                trust: Arc::new(TestTrust::default()),
                settings,
                events: Arc::default(),
                fail: Arc::default(),
                installer: false,
                downloads: Arc::default(),
                started: Arc::default(),
                portable: None,
                data: None,
                mid_download: None,
                n: None,
            };
            e.n = Some(e.open(kind));
            e
        }

        /// The MSI with its installer, as the service runs.
        fn installing() -> Env {
            let mut e = Env::new(Mode::Notify, InstallKind::Msi);
            e.installer = true;
            e.n = Some(e.open(InstallKind::Msi));
            e
        }

        fn open(&self, kind: InstallKind) -> Notifier {
            let sink = self.events.clone();
            let keys = self.trust.clone();
            let fail = self.fail.clone();
            Notifier::with(
                Context {
                    data_dir: self
                        .data
                        .clone()
                        .unwrap_or_else(|| self.dir.path().to_path_buf()),
                    running: Version::new(0, 5, 3),
                    repository: "https://github.com/dvortsis/lodestar-v2".into(),
                    dev: false,
                    kind,
                    target: WIN,
                    checked_data: true,
                    db: self
                        .data
                        .clone()
                        .unwrap_or_else(|| self.dir.path().to_path_buf())
                        .join("lodestar.db"),
                },
                self.settings.subscribe(),
                Arc::new(move |e| lock(&sink).push(e)),
                self.fake.clone(),
                (jiff::tz::TimeZone::UTC, "UTC".into()),
                Seams {
                    clock: Box::new(|| NOW),
                    keys: Box::new(move || Ok(keys.embedded(vec![]))),
                    fault: Box::new(move |step| match *lock(&fail) {
                        Some(f) if f == step => Err(std::io::Error::other("injected")),
                        _ => Ok(()),
                    }),
                    installer: self.installer.then(|| {
                        let started = self.started.clone();
                        let source: Arc<dyn crate::update::download::Source> =
                            match &self.mid_download {
                                Some(n) => Arc::new(Hooked {
                                    inner: self.downloads.clone(),
                                    notifier: n.clone(),
                                }),
                                None => self.downloads.clone(),
                            };
                        super::super::msi::Installer {
                            source,
                            start: Box::new(move || {
                                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                Ok(())
                            }),
                        }
                    }),
                    portable: self.portable.clone(),
                },
            )
        }

        fn bin_file(&self, name: &str) -> PathBuf {
            self.dir.path().join("bin").join(name)
        }

        fn notices(json: &Value) -> Vec<&str> {
            json["notices"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap())
                .collect()
        }

        fn n(&self) -> &Notifier {
            self.n.as_ref().unwrap()
        }

        /// Serves `releases` on `channel`'s manifest, at the Stable URL.
        fn serve(&self, channel: Channel, releases: Vec<Value>) {
            let mut m = manifest(channel, NOW as u64, "0.0.1", b"x");
            m["releases"] = Value::Array(releases);
            let signed = self.trust.release.signed(&bytes(&m));
            self.fake.serve(STABLE, 200, None, &envelope(&signed, None));
        }

        async fn check(&self) -> Value {
            serde_json::to_value(self.n().check_now().await.unwrap()).unwrap()
        }

        fn status(&self) -> Value {
            serde_json::to_value(self.n().status()).unwrap()
        }

        fn states(&self) -> Vec<(UpdatesState, Option<String>)> {
            lock(&self.events)
                .iter()
                .map(|e| (e.state, e.version.clone()))
                .collect()
        }

        fn versions(json: &Value) -> Vec<&str> {
            json["releases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["version"].as_str().unwrap())
                .collect()
        }
    }

    /// The `/updates` contract on Windows: no `unavailable`, the releases
    /// a check found for this installation, nothing installable (even
    /// under `auto`), and Install and Roll back refused with
    /// `not_built_yet`.
    #[tokio::test]
    async fn checks_and_notifies_but_installs_nothing() {
        let e = Env::new(Mode::Auto, InstallKind::Msi);
        e.serve(
            Channel::Stable,
            vec![
                release_with("0.5.5", &["binary"]),
                release_with("0.5.4", &["binary", "msi", "portable"]),
            ],
        );
        let json = e.check().await;
        assert_eq!(json["unavailable"], Value::Null);
        assert_eq!(json["status"]["state"], "ok");
        assert_eq!(json["status"]["checked_at"], NOW);
        assert_eq!(Env::versions(&json), ["0.5.4"], "the Linux-only one isn't");
        assert_eq!(json["capability"]["can_install"], false);
        assert_eq!(json["capability"]["reason"], "not_built_yet");
        let r = &json["releases"][0];
        assert_eq!(r["eligible"], false);
        assert_eq!(r["installs_at"], Value::Null);
        assert!(
            r["blocked_by"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["code"] == "capability" && b["reason"] == "not_built_yet"),
            "{r}"
        );
        assert_eq!(json["rollback"]["available"], false);
        assert_eq!(json["rollback"]["reason"], "not_built_yet");
        assert_eq!(json["running"]["version"], "0.5.3");
        assert_eq!(json["running"]["channel"], "stable");
        assert_eq!(json["policy"]["mode"], "auto");
        assert_eq!(json["pending"], Value::Null);
        assert_eq!(json["history"], json!([]));
        assert_eq!(
            e.states(),
            [
                (UpdatesState::Checking, None),
                (UpdatesState::Available, Some("0.5.4".into()))
            ]
        );
        // The times and the trust state are saved.
        let bin = e.dir.path().join("bin");
        let u: Updater =
            serde_json::from_slice(&std::fs::read(bin.join(UPDATER_FILE)).unwrap()).unwrap();
        assert_eq!(u.checked_at, Some(NOW));
        assert!(u.next_check_at.unwrap() >= NOW + EVERY.as_secs() as i64);
        assert!(bin.join(trust_file::FILE).exists());
        // Install and Roll back say why not.
        let n = e.n();
        assert_eq!(
            UpdatesApi::install(n, "0.5.4".into(), InstallWhen::Now, true)
                .await
                .unwrap_err(),
            UpdatesError::InstallUnavailable(NOT_BUILT_YET.code())
        );
        assert_eq!(
            UpdatesApi::rollback(n).await.unwrap_err(),
            UpdatesError::RollbackUnavailable(NOT_BUILT_YET.code())
        );
        assert!(UpdatesApi::clear_history(n).await.is_ok());
        // A second manual check within the minute waits.
        assert!(matches!(
            n.check_now().await.unwrap_err(),
            UpdatesError::RateLimited { .. }
        ));
    }

    #[tokio::test]
    async fn each_kind_is_offered_what_it_can_take() {
        let releases = || {
            vec![
                release_with("0.5.6", &["portable"]),
                release_with("0.5.5", &["msi"]),
                release_with("0.5.4", &["binary"]),
            ]
        };
        for (kind, want) in [
            (InstallKind::Msi, vec!["0.5.5"]),
            (InstallKind::Portable, vec!["0.5.6"]),
            (InstallKind::Unknown, vec!["0.5.6", "0.5.5"]),
        ] {
            let e = Env::new(Mode::Notify, kind);
            e.serve(Channel::Stable, releases());
            assert_eq!(Env::versions(&e.check().await), want, "{kind:?}");
        }
    }

    /// A validly signed manifest for another channel is refused: nothing
    /// is offered and the check fails.
    #[tokio::test]
    async fn a_manifest_for_another_channel_is_refused() {
        let e = Env::new(Mode::Notify, InstallKind::Msi);
        e.serve(Channel::Beta, vec![release_with("0.5.4", &["msi"])]);
        let json = e.check().await;
        assert_eq!(json["status"]["state"], "failed");
        assert!(json["status"]["error"].is_string());
        assert_eq!(json["releases"], json!([]));
        assert_eq!(e.states().last().unwrap().0, UpdatesState::Failed);
    }

    #[tokio::test]
    async fn nothing_published_yet_is_a_good_check() {
        let e = Env::new(Mode::Notify, InstallKind::Msi);
        let json = e.check().await;
        assert_eq!(json["status"]["state"], "ok");
        assert_eq!(json["releases"], json!([]));
        assert_eq!(e.states().last().unwrap().0, UpdatesState::Checked);
    }

    #[tokio::test]
    async fn skip_and_pause_are_saved() {
        let mut e = Env::new(Mode::Notify, InstallKind::Msi);
        e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
        e.check().await;
        assert_eq!(
            e.n().skip("0.9.9").unwrap_err(),
            UpdatesError::UnknownVersion
        );
        let skipped = serde_json::to_value(e.n().skip("0.5.4").unwrap()).unwrap();
        assert_eq!(skipped["releases"], json!([]));
        let paused = serde_json::to_value(e.n().pause(7).unwrap()).unwrap();
        assert_eq!(paused["policy"]["paused_until"], NOW + 7 * 86_400);
        // Both survive a restart (the lock goes with the old notifier).
        e.n = None;
        e.n = Some(e.open(InstallKind::Msi));
        assert_eq!(e.status()["policy"]["paused_until"], NOW + 7 * 86_400);
        let list: Value = serde_json::from_slice(
            &std::fs::read(e.dir.path().join("bin").join(SKIPPED_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(list, json!([{"version": "0.5.4", "reason": "user"}]));
        assert!(e.n().pause(0).is_ok());
        assert_eq!(e.status()["policy"]["paused_until"], Value::Null);
    }

    /// Another process holding `<data>\bin\.lock` keeps this one from
    /// fetching or writing anything, until it lets go.
    #[tokio::test]
    async fn a_second_writer_gets_nothing_until_the_lock_is_free() {
        let mut e = Env::new(Mode::Notify, InstallKind::Msi);
        e.n = None;
        let first = Bin::open(e.dir.path()).unwrap();
        let held = first.lock().unwrap().unwrap();
        e.n = Some(e.open(InstallKind::Msi));
        e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
        let json = e.check().await;
        assert_eq!(json["status"]["error"], "bin_unavailable");
        assert!(e.fake.asked.lock().unwrap().is_empty(), "nothing fetched");
        assert_eq!(e.n().pause(1).unwrap_err(), UpdatesError::BinUnavailable);
        assert!(!e.bin_file(UPDATER_FILE).exists());
        // Once it's free, the next write takes it.
        drop(held);
        drop(first);
        let paused = serde_json::to_value(e.n().pause(1).unwrap()).unwrap();
        assert_eq!(paused["policy"]["paused_until"], NOW + 86_400);
        assert!(e.bin_file(UPDATER_FILE).exists());
    }

    /// A write that fails isn't acknowledged and changes nothing: a skip
    /// or pause answers `not_saved`, and a check's times stay in memory.
    #[tokio::test]
    async fn a_failed_write_is_not_acknowledged() {
        let e = Env::new(Mode::Notify, InstallKind::Msi);
        e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
        *lock(&e.fail) = Some(Step::Rename);
        let json = e.check().await;
        assert_eq!(json["status"]["checked_at"], NOW, "kept in memory");
        assert!(Env::notices(&json).contains(&"check_not_saved"), "{json}");
        assert!(!e.bin_file(UPDATER_FILE).exists());
        assert_eq!(e.n().skip("0.5.4").unwrap_err(), UpdatesError::NotSaved);
        assert_eq!(Env::versions(&e.status()), ["0.5.4"]);
        assert!(!e.bin_file(SKIPPED_FILE).exists());
        assert_eq!(e.n().pause(7).unwrap_err(), UpdatesError::NotSaved);
        assert_eq!(e.status()["policy"]["paused_until"], Value::Null);
        // Every step, not only the rename.
        for step in [Step::Write, Step::Flush, Step::SyncDir] {
            *lock(&e.fail) = Some(step);
            assert_eq!(
                e.n().pause(7).unwrap_err(),
                UpdatesError::NotSaved,
                "{step:?}"
            );
        }
        *lock(&e.fail) = None;
        assert!(e.n().pause(7).is_ok());
        assert_eq!(e.status()["policy"]["paused_until"], NOW + 7 * 86_400);
        assert!(e.bin_file(UPDATER_FILE).exists());
    }

    /// A `skipped` that can't be read (damaged, oversize) blocks rather
    /// than forgets a skip, and is never written over.
    #[tokio::test]
    async fn an_unreadable_skip_list_is_kept_and_named() {
        for bad in [b"{ not a list".to_vec(), vec![b' '; 256 * 1024 + 1]] {
            let e = Env::new(Mode::Notify, InstallKind::Msi);
            std::fs::write(e.bin_file(SKIPPED_FILE), &bad).unwrap();
            e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
            let json = e.check().await;
            assert_eq!(json["capability"]["reason"], "skipped_unreadable");
            assert!(matches!(
                e.n().skip("0.5.4").unwrap_err(),
                UpdatesError::Internal(_)
            ));
            assert_eq!(std::fs::read(e.bin_file(SKIPPED_FILE)).unwrap(), bad);
        }
    }

    /// `skipped` keeps the newest 64, dropping the oldest.
    #[tokio::test]
    async fn the_skip_list_keeps_the_newest_64() {
        let e = Env::new(Mode::Notify, InstallKind::Msi);
        let old: Vec<Skip> = (0..MAX_SKIPPED)
            .map(|i| Skip {
                version: format!("0.1.{i}"),
                reason: "user".into(),
            })
            .collect();
        std::fs::write(e.bin_file(SKIPPED_FILE), serde_json::to_vec(&old).unwrap()).unwrap();
        e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
        e.check().await;
        e.n().skip("0.5.4").unwrap();
        let list: Vec<Skip> =
            serde_json::from_slice(&std::fs::read(e.bin_file(SKIPPED_FILE)).unwrap()).unwrap();
        assert_eq!(list.len(), MAX_SKIPPED);
        assert_eq!(list[0].version, "0.1.1", "the oldest went");
        assert_eq!(list.last().unwrap().version, "0.5.4");
    }

    /// The helper's `failed` entry survives any number of user skips, and
    /// a user skip never replaces it.
    #[tokio::test]
    async fn user_skips_never_push_out_a_failed_one() {
        let e = Env::new(Mode::Notify, InstallKind::Msi);
        let bin = Bin::open(e.dir.path()).unwrap();
        add_skip(&bin, "0.0.9", SKIP_FAILED).unwrap();
        for i in 0..MAX_SKIPPED {
            add_skip(&bin, &format!("0.1.{i}"), "user").unwrap();
        }
        let list: Vec<Skip> = bin.read(SKIPPED_FILE).unwrap().unwrap();
        assert_eq!(list.len(), MAX_SKIPPED);
        assert_eq!(
            (list[0].version.as_str(), list[0].reason.as_str()),
            ("0.0.9", "failed")
        );
        assert_eq!(list[1].version, "0.1.1", "the oldest user skip went");
        // A user skip of the failed version doesn't replace it.
        add_skip(&bin, "0.0.9", "user").unwrap();
        let again: Vec<Skip> = bin.read(SKIPPED_FILE).unwrap().unwrap();
        assert_eq!(again, list);
        // Only failed entries over the limit: all kept.
        let mut only: Vec<Skip> = (0..MAX_SKIPPED + 2)
            .map(|i| Skip {
                version: format!("0.2.{i}"),
                reason: SKIP_FAILED.into(),
            })
            .collect();
        trim(&mut only);
        assert_eq!(only.len(), MAX_SKIPPED + 2);
    }

    /// A damaged `trust.json` is named and never written over; the check
    /// still runs from the embedded floor.
    #[tokio::test]
    async fn a_damaged_trust_file_is_named_and_kept() {
        let e = Env::new(Mode::Notify, InstallKind::Msi);
        std::fs::write(e.bin_file(trust_file::FILE), b"{ planted").unwrap();
        e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
        let json = e.check().await;
        assert_eq!(json["status"]["state"], "ok");
        assert!(Env::notices(&json).contains(&"trust_damaged"), "{json}");
        assert_eq!(Env::versions(&json), ["0.5.4"]);
        assert_eq!(
            std::fs::read(e.bin_file(trust_file::FILE)).unwrap(),
            b"{ planted"
        );
    }

    /// An `updater.json` that can't be read (planted, damaged) isn't
    /// trusted and isn't overwritten by a check; a pause replaces it.
    #[tokio::test]
    async fn an_unreadable_updater_file_is_kept_until_a_pause() {
        let mut e = Env::new(Mode::Notify, InstallKind::Msi);
        e.n = None;
        let path = e.dir.path().join("bin").join(UPDATER_FILE);
        std::fs::write(&path, br#"{"format":1,"surprise":true}"#).unwrap();
        e.n = Some(e.open(InstallKind::Msi));
        let json = e.check().await;
        assert!(
            json["notices"]
                .as_array()
                .unwrap()
                .contains(&json!("updater_unreadable"))
        );
        assert!(
            json["notices"]
                .as_array()
                .unwrap()
                .contains(&json!("check_not_saved"))
        );
        assert_eq!(json["status"]["checked_at"], NOW, "kept in memory");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            br#"{"format":1,"surprise":true}"#
        );
        assert!(e.n().pause(0).is_ok());
        let u: Updater = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(u.paused_until, None);
        assert!(
            !e.status()["notices"]
                .as_array()
                .unwrap()
                .contains(&json!("updater_unreadable"))
        );
    }

    /// The schedule checks at once when no check is on record, and not at
    /// all with updates off.
    #[tokio::test]
    async fn the_schedule_runs_the_first_check_and_stops() {
        let e = Env::new(Mode::Notify, InstallKind::Msi);
        assert_eq!(e.n().until_scheduled(), Some(Duration::ZERO));
        let (stop, stopped) = watch::channel(false);
        let task = e.n().spawn(stopped);
        let start = Instant::now();
        while e.n().updater().checked_at.is_none() {
            assert!(start.elapsed() < Duration::from_secs(20), "no check ran");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(e.n().summary(), (true, Some(NOW)));
        stop.send(true).unwrap();
        task.await.unwrap();
        e.settings.send_modify(|s| s.mode = Mode::Off);
        assert_eq!(e.n().until_scheduled(), None);
        assert_eq!(e.status()["status"]["next_check_at"], Value::Null);
    }

    /// The same files as the Unix engine, both ways.
    #[test]
    fn updater_json_reads_the_unix_engines_fields() {
        let u: Updater = serde_json::from_str(
            r#"{"format":1,"checked_at":1,"next_check_at":2,"paused_until":3,"error":null,
                "queued":"0.5.4","rolled_back_seen":9}"#,
        )
        .unwrap();
        assert_eq!(u.queued.as_deref(), Some("0.5.4"));
        assert!(serde_json::from_str::<Updater>(r#"{"format":1,"x":1}"#).is_err());
    }

    /// Install (the MSI): the pinned MSI is downloaded, handed over with
    /// the envelope and a request, and the helper's task started.
    #[tokio::test]
    async fn install_hands_the_msi_to_the_helper() {
        let e = Env::installing();
        e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
        let url = "https://github.com/dvortsis/lodestar-v2/releases/download/v0.5.4/lodestar.msi";
        e.downloads.serve(url, 200, None, b"x");
        let status = e.check().await;
        assert_eq!(status["capability"]["can_install"], true, "{status}");
        assert_eq!(status["capability"]["reason"], Value::Null);
        let answer = e
            .n()
            .install_request("0.5.4".into(), InstallWhen::Now, false)
            .await
            .unwrap();
        assert_eq!(answer.installing.unwrap().version, "0.5.4");
        // The background half: wait for the helper's start, with a deadline.
        let deadline = Instant::now() + Duration::from_secs(10);
        while e.started.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the helper wasn't started");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let updates = e.dir.path().join(super::super::msi::UPDATES);
        assert_eq!(
            std::fs::read(updates.join("lodestar-0.5.4.msi")).unwrap(),
            b"x"
        );
        let req = std::fs::read(updates.join(super::super::msi::REQUEST)).unwrap();
        let (r, v) = super::super::msi::parse_request(&req).unwrap();
        assert_eq!((r.channel, v), (Channel::Stable, Version::new(0, 5, 4)));
        // The envelope is the one checked, byte for byte.
        let envelope = std::fs::read(updates.join(super::super::msi::ENVELOPE)).unwrap();
        assert_eq!(Some(envelope), lock(&e.n().inner.found).envelope.clone());
        assert!(
            e.states()
                .contains(&(UpdatesState::Installing, Some("0.5.4".into())))
        );
    }

    /// The portable install's system side, for the notifier's tests.
    struct PortableSys {
        program: PathBuf,
        private: std::sync::atomic::AtomicBool,
        restarts: std::sync::atomic::AtomicUsize,
    }

    impl super::super::portable::Sys for PortableSys {
        fn program(&self) -> PathBuf {
            self.program.clone()
        }
        fn version(&self) -> String {
            "0.5.3".into()
        }
        fn bin_private(&self, _: &Path) -> Result<(), String> {
            if self.private.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err("shared".into())
            }
        }
        fn create_bin(&self, dir: &Path) -> std::io::Result<()> {
            std::fs::create_dir(dir)
        }
        fn restart(&self) -> std::io::Result<()> {
            self.restarts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    /// The portable build: Install offered only with a private `bin`, the
    /// zip downloaded and handed to its own steps, which refuse a zip that
    /// isn't the release's; nothing restarts and the program stays.
    #[tokio::test]
    async fn the_portable_build_installs_through_its_own_steps() {
        let mut e = Env::new(Mode::Notify, InstallKind::Portable);
        let app = e.dir.path().join("app");
        std::fs::create_dir_all(app.join("data")).unwrap();
        std::fs::write(app.join("lodestar.exe"), b"old").unwrap();
        let sys = Arc::new(PortableSys {
            program: app.join("lodestar.exe"),
            private: true.into(),
            restarts: Default::default(),
        });
        e.installer = true;
        e.portable = Some(sys.clone());
        e.data = Some(app.join("data"));
        e.n = None;
        // While another process holds `<data>\bin\.lock`, it isn't offered.
        {
            let other = crate::update::layout::Bin::open(&app.join("data")).unwrap();
            let held = other.lock().unwrap().unwrap();
            let busy = e.open(InstallKind::Portable);
            let reason =
                serde_json::to_value(busy.status()).unwrap()["capability"]["reason"].clone();
            assert_eq!(reason, "bin_unavailable");
            drop(busy);
            drop(held);
        }
        e.n = Some(e.open(InstallKind::Portable));
        e.serve(Channel::Stable, vec![release_with("0.5.5", &["portable"])]);
        let url = "https://github.com/dvortsis/lodestar-v2/releases/download/v0.5.5/lodestar.zip";
        e.downloads.serve(url, 200, None, b"x");
        let status = e.check().await;
        assert_eq!(status["capability"]["reason"], Value::Null, "{status}");
        sys.private
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(e.status()["capability"]["reason"], "bin_not_private");
        let refused = e
            .n()
            .install_request("0.5.5".into(), InstallWhen::Now, false)
            .await;
        assert!(refused.is_err());
        sys.private.store(true, std::sync::atomic::Ordering::SeqCst);
        e.n()
            .install_request("0.5.5".into(), InstallWhen::Now, false)
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let failed = || {
            lock(&e.events).iter().any(|ev| {
                ev.state == UpdatesState::Failed && ev.error.as_deref() == Some("zip_refused")
            })
        };
        while !failed() {
            assert!(Instant::now() < deadline, "no failure: {:?}", e.states());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(std::fs::read(app.join("lodestar.exe")).unwrap(), b"old");
        assert!(!app.join("data").join("bin").join("portable.json").exists());
        assert_eq!(sys.restarts.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            e.downloads
                .asked
                .lock()
                .unwrap()
                .iter()
                .any(|u| u.contains("lodestar.zip"))
        );
    }

    /// The helper's last answer, for a newer version: a blocked install
    /// has its own notice, any other miss `install_failed`, an install or
    /// an older version none.
    #[tokio::test]
    async fn a_blocked_install_has_its_own_notice() {
        let e = Env::installing();
        let updates = e.dir.path().join(super::super::msi::UPDATES);
        std::fs::create_dir_all(&updates).unwrap();
        let answer = |outcome: &str, version: &str| {
            let a = json!({"format": 1, "id": "ab".repeat(16), "version": version,
                "outcome": outcome, "at": 1});
            std::fs::write(updates.join(super::super::msi::RESULT), a.to_string()).unwrap();
            Env::notices(&e.status())
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
        };
        let has = |n: &[String], c: &str| n.iter().any(|x| x == c);
        let pending = |id: &str| {
            let r = json!({"format": 1, "id": id, "version": "0.5.4", "channel": "stable"});
            std::fs::write(updates.join(super::super::msi::REQUEST), r.to_string()).unwrap();
        };
        // Blocked, with its request still waiting for the retry.
        pending(&"ab".repeat(16));
        let n = answer("install_blocked", "0.5.4");
        assert!(
            has(&n, "install_blocked") && !has(&n, "install_failed"),
            "{n:?}"
        );
        // The same while updates are already Off: switching off or pausing
        // can't cancel it from there, so it has words of its own.
        e.settings.send_modify(|s| s.mode = Mode::Off);
        let n = answer("install_blocked", "0.5.4");
        assert!(
            has(&n, "install_blocked_off") && !has(&n, "install_blocked"),
            "{n:?}"
        );
        e.settings.send_modify(|s| s.mode = Mode::Notify);
        // Blocked, but its request was taken back (Off, a pause) or is
        // another one: no retry is coming, so no "tries again".
        for other in [None, Some("cd".repeat(16))] {
            match &other {
                Some(id) => pending(id),
                None => std::fs::remove_file(updates.join(super::super::msi::REQUEST)).unwrap(),
            }
            let n = answer("install_blocked", "0.5.4");
            assert!(
                has(&n, "install_failed") && !has(&n, "install_blocked"),
                "{other:?}: {n:?}"
            );
        }
        let n = answer("install_failed", "0.5.4");
        assert!(
            has(&n, "install_failed") && !has(&n, "install_blocked"),
            "{n:?}"
        );
        for (outcome, version) in [("installed", "0.5.4"), ("install_blocked", "0.5.3")] {
            let n = answer(outcome, version);
            assert!(
                !has(&n, "install_blocked") && !has(&n, "install_failed"),
                "{n:?}"
            );
        }
    }

    // ---- Install now respects Off and a pause (#677's flag) ----

    const MSI_URL: &str =
        "https://github.com/dvortsis/lodestar-v2/releases/download/v0.5.4/lodestar.msi";

    fn request(e: &Env) -> PathBuf {
        e.dir
            .path()
            .join(super::super::msi::UPDATES)
            .join(super::super::msi::REQUEST)
    }

    /// Install now, handed to the helper: the request waits in `updates\`.
    async fn handed_over(e: &Env) {
        e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
        e.downloads.serve(MSI_URL, 200, None, b"x");
        e.check().await;
        e.n()
            .install_request("0.5.4".into(), InstallWhen::Now, false)
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while e.started.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the helper wasn't started");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(request(e).exists());
    }

    /// Updates switched Off: the waiting request goes, so the helper's
    /// boot retry has nothing to replay.
    #[tokio::test]
    async fn switching_updates_off_takes_back_a_waiting_install() {
        let e = Env::installing();
        handed_over(&e).await;
        let (stop, stopped) = watch::channel(false);
        let task = e.n().spawn(stopped);
        // Another setting changing leaves it.
        e.settings.send_modify(|s| s.level = Level::All);
        e.n().pause(0).unwrap();
        e.settings.send_modify(|s| s.mode = Mode::Off);
        let deadline = Instant::now() + Duration::from_secs(20);
        while request(&e).exists() {
            assert!(Instant::now() < deadline, "the request stayed after Off");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.send(true).unwrap();
        task.await.unwrap();
    }

    /// A setting other than the mode, Off staying Off, or a resume
    /// leaves the request; only the switch to Off takes it.
    #[tokio::test]
    async fn only_a_switch_to_off_or_a_pause_takes_it_back() {
        let e = Env::installing();
        handed_over(&e).await;
        let mut was_off = false;
        e.settings.send_modify(|s| s.level = Level::All);
        e.n().settings_changed(&mut was_off);
        e.n().pause(0).unwrap();
        assert!(request(&e).exists());
        e.settings.send_modify(|s| s.mode = Mode::Off);
        e.n().settings_changed(&mut was_off);
        assert!(!request(&e).exists());
        // Off staying Off: an Install now made meanwhile stays.
        std::fs::write(request(&e), b"{}").unwrap();
        e.settings.send_modify(|s| s.level = Level::Fixes);
        e.n().settings_changed(&mut was_off);
        assert!(request(&e).exists());
    }

    /// A pause starting takes it back at once.
    #[tokio::test]
    async fn a_pause_takes_back_a_waiting_install() {
        let e = Env::installing();
        handed_over(&e).await;
        e.n().pause(7).unwrap();
        assert!(!request(&e).exists());
    }

    /// At the notifier's start, a waiting request stays, Off or paused:
    /// only a switch to Off or a pause starting takes it back, so an
    /// Install now made while Off keeps its retry across a restart.
    #[tokio::test]
    async fn a_request_waiting_at_start_stays() {
        let mut e = Env::new(Mode::Off, InstallKind::Msi);
        e.installer = true;
        e.n = None;
        e.n = Some(e.open(InstallKind::Msi));
        handed_over(&e).await;
        let (stop, stopped) = watch::channel(false);
        let task = e.n().spawn(stopped);
        // Another setting changing while Off is no switch either: the real
        // watcher handles it, and the request stays.
        let handled = || {
            e.n()
                .inner
                .handled
                .load(std::sync::atomic::Ordering::SeqCst)
        };
        e.settings.send_modify(|s| s.level = Level::All);
        let deadline = Instant::now() + Duration::from_secs(20);
        while handled() == 0 {
            assert!(
                Instant::now() < deadline,
                "the watcher didn't handle the change"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(request(&e).exists());
        stop.send(true).unwrap();
        task.await.unwrap();
        assert!(request(&e).exists());
    }

    /// A switch to Off sent just before the stop still takes the request
    /// back: the watcher handles a waiting change first, and once more at
    /// the stop.
    #[tokio::test]
    async fn a_switch_off_just_before_a_stop_still_withdraws() {
        let e = Env::installing();
        handed_over(&e).await;
        let (stop, stopped) = watch::channel(false);
        let task = e.n().spawn(stopped);
        e.settings.send_modify(|s| s.mode = Mode::Off);
        stop.send(true).unwrap();
        // The task waits for the watcher's last look.
        task.await.unwrap();
        assert!(!request(&e).exists(), "Off just before the stop was lost");
    }

    /// A switch to Off is acted on while a check or an install holds the
    /// check lock: the watcher doesn't wait for it. The scheduled check is
    /// due here, so the check loop is parked on the lock the test holds;
    /// and the loop never withdraws, so the removal can only be the
    /// watcher's.
    #[tokio::test]
    async fn a_switch_off_is_seen_while_the_check_lock_is_held() {
        let e = Env::installing();
        handed_over(&e).await;
        let held = e.n().inner.checking.clone().lock_owned().await;
        let (stop, stopped) = watch::channel(false);
        let task = e.n().spawn(stopped);
        e.settings.send_modify(|s| s.mode = Mode::Off);
        let deadline = Instant::now() + Duration::from_secs(20);
        while request(&e).exists() {
            assert!(Instant::now() < deadline, "Off waited for the check lock");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(held);
        stop.send(true).unwrap();
        task.await.unwrap();
    }

    /// Off arriving while the install downloads: its request is taken
    /// back after it's written, and the helper isn't started.
    #[tokio::test]
    async fn a_switch_off_during_the_download_takes_the_request_back() {
        let mut e = Env::new(Mode::Notify, InstallKind::Msi);
        e.installer = true;
        let hook = Arc::new(std::sync::OnceLock::new());
        e.mid_download = Some(hook.clone());
        e.n = None;
        e.n = Some(e.open(InstallKind::Msi));
        hook.set(e.n().clone()).unwrap();
        e.serve(Channel::Stable, vec![release_with("0.5.4", &["msi"])]);
        e.downloads.serve(MSI_URL, 200, None, b"x");
        e.check().await;
        e.n()
            .install_request("0.5.4".into(), InstallWhen::Now, false)
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let withdrawn = || {
            lock(&e.events).iter().any(|ev| {
                ev.state == UpdatesState::Failed && ev.error.as_deref() == Some("withdrawn")
            })
        };
        while !withdrawn() {
            assert!(Instant::now() < deadline, "no withdrawal: {:?}", e.states());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!request(&e).exists());
        assert_eq!(e.started.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Install now asked for while updates are already Off still goes
    /// ahead: only a switch to Off or a pause starting takes it back.
    #[tokio::test]
    async fn install_now_while_off_still_installs() {
        let mut e = Env::new(Mode::Off, InstallKind::Msi);
        e.installer = true;
        e.n = None;
        e.n = Some(e.open(InstallKind::Msi));
        let (stop, stopped) = watch::channel(false);
        let task = e.n().spawn(stopped);
        handed_over(&e).await;
        assert!(request(&e).exists());
        stop.send(true).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn install_refuses_what_it_mustnt() {
        let e = Env::installing();
        e.serve(
            Channel::Stable,
            vec![
                release_with("0.5.4", &["msi"]),
                release_with("0.5.2", &["msi"]),
                release_with("0.5.5", &["portable"]),
            ],
        );
        e.check().await;
        let ask = |v: &str, when| e.n().install_request(v.into(), when, false);
        let err = |r: Result<UpdatesStatus, UpdatesError>| format!("{:?}", r.unwrap_err());
        assert_eq!(
            err(ask("0.5.2", InstallWhen::Now).await),
            "InstallUnavailable(\"not_candidate\")"
        );
        assert_eq!(err(ask("0.5.5", InstallWhen::Now).await), "UnknownVersion");
        assert_eq!(err(ask("0.9.9", InstallWhen::Now).await), "UnknownVersion");
        assert_eq!(
            err(ask("0.5.4", InstallWhen::Window).await),
            "InstallUnavailable(\"not_built_yet\")"
        );
        e.n().skip("0.5.4").unwrap();
        assert_eq!(err(ask("0.5.4", InstallWhen::Now).await), "VersionSkipped");
        assert_eq!(e.started.load(std::sync::atomic::Ordering::SeqCst), 0);
        // An unknown kind, the portable build or a moved data folder aren't
        // offered Install.
        for (kind, reason) in [
            (InstallKind::Unknown, "install_kind_unknown"),
            (InstallKind::Portable, "not_built_yet"),
        ] {
            let mut e = Env::new(Mode::Notify, kind);
            e.installer = true;
            e.n = Some(e.open(kind));
            assert_eq!(e.status()["capability"]["reason"], reason);
        }
    }
}
