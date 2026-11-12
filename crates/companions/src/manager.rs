//! The companion manager (plan 2026-11-06-v06-companions §4.3 CMP-5, Q6,
//! Q17; architecture §18.10; decisions `docs/decisions/2026-11-08-CMP-5.md`).
//!
//! **Install** is one state machine journalled in the companion's
//! [`state`] record:
//! confirm → fetch, verify and extract (`staged`) → credentials → config
//! files → commit (the pointer) → start and health (`healthy`) →
//! configure through the API (`configured`, the commit point) → done.
//! Each step's effect is safe to find half-done: a crash anywhere before
//! `configured` is rolled back at start-up ([`Manager::recover`]) to the
//! old version, or to nothing for a first install; one after it is rolled
//! forward, which only deletes the previous version.
//!
//! **Update** (Q6) is the same with the companion's own credential and
//! config kept: the previous version stays on disk, and in the pointer,
//! until the new one passes health and is configured. A failed update is
//! rolled back and the previous version started again.
//!
//! **Remove** stops the companion, deletes its credential rows and then
//! `<data>/companions/<id>/` ([`layout::Install::remove`]), nothing else.
//! [`Manager::removal`] says exactly what goes, before you confirm.
//!
//! **Refusals** are typed ([`Refusal`]): a native install inside a
//! container (Q17), an unsupported platform, a build that isn't pinned,
//! the Windows installer (guided, CMP-11), and a missing or wrong
//! confirmation. Nothing runs that CMP-1 didn't verify against the pin.
//!
//! The effects outside this crate's files go through traits, so tests
//! fake them: [`Rows`] (credential rows in `settings`), [`Run`] (start,
//! health, stop; [`Supervised`] is the real one) and [`ApiConfigure`]
//! (CMP-8b; [`Api`] is the real one).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_integrations::secret::{SealedSecret, Secret, SecretStore};
use lodestar_sources::egress::{DownReason, PluginEgress, RouteKind, RouteState};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::adapter::CredentialError;
use crate::adapter::{AdapterError, BoxFuture, Credentials, GuardedHttp, HealthStatus, Target};
use crate::catalogue::{Arch, ArchiveKind, Artifact, Catalogue, Entry, NotAvailable, Pin};
use crate::catalogue::{Platform, RelPath, Sha256};
use crate::config_files::{self, Username};
use crate::configure::{self, InstallReceipt};
use crate::credentials::{self, CredentialKey, Field, SealedRow};
use crate::fetch::{self, FetchError, Guarded, Verified};
use crate::inputs::{CompanionId, Version};
use crate::layout::{Install, LayoutError};
use crate::log_events::{INSTALL_FAILED, INSTALLED, RECOVERED, REMOVED, ROLLED_BACK};
use crate::state::{self, Op, OpKind, Record, StateError, Step};
use crate::supervise::egress::{self, Follows, Gated, Route};
use crate::supervise::{
    Companion, Confinement, EnvName, LogRing, Probe, Sandbox, SpawnSpec, State, Status, Supervisor,
};
use crate::verify;

/// A test's fault point (`Fault::$f`); nothing in other builds.
macro_rules! fault {
    ($m:expr, $($f:tt)+) => {
        #[cfg(any(test, feature = "testing"))]
        $m.point(Fault::$($f)+)?;
    };
}

/// Where Lodestar runs, for the refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Host {
    /// `None` on a platform the catalogue has no builds for.
    pub platform: Option<(Platform, Arch)>,
    /// Inside Docker, Unraid or another container (Q17).
    pub in_container: bool,
}

impl Host {
    /// This build's platform; `in_container` from the caller's detection.
    pub fn this(in_container: bool) -> Host {
        let os = if cfg!(target_os = "linux") {
            Some(Platform::Linux)
        } else if cfg!(windows) {
            Some(Platform::Windows)
        } else {
            None
        };
        let arch = if cfg!(target_arch = "x86_64") {
            Some(Arch::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Arch::Aarch64)
        } else {
            None
        };
        Host {
            platform: os.zip(arch),
            in_container,
        }
    }
}

/// Why the manager won't do it. Nothing was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// Inside a container only the guided path is offered (Q17).
    InContainer,
    UnsupportedPlatform,
    /// Not in the catalogue.
    Unknown,
    NotAvailable(NotAvailable),
    /// The Windows installer is guided (CMP-11), never run by Lodestar.
    GuidedOnly,
    /// No launch profile for it in this build (FlareSolverr is CMP-12's).
    NoProfile,
    /// The confirmation doesn't name this entry's pinned version and
    /// SHA-256.
    Confirmation,
    /// Another operation on it is under way.
    Busy,
    /// That version is already installed.
    Current,
    /// It isn't installed by Lodestar.
    NotInstalled,
    /// A credential is stored for a companion Lodestar didn't install (a
    /// detected or guided one): remove that first.
    Present,
    /// `<data>/companions/<id>/` holds files Lodestar has no record of
    /// (a compose companion's host folder can sit there): it's never
    /// installed over or deleted.
    FolderInUse,
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::InContainer => "native_in_container",
            Refusal::UnsupportedPlatform => "unsupported_platform",
            Refusal::Unknown => "unknown_companion",
            Refusal::NotAvailable(NotAvailable::AwaitingPin) => "awaiting_pin",
            Refusal::NotAvailable(_) => "no_build",
            Refusal::GuidedOnly => "guided_only",
            Refusal::NoProfile => "no_launch_profile",
            Refusal::Confirmation => "confirmation_mismatch",
            Refusal::Busy => "busy",
            Refusal::Current => "already_current",
            Refusal::NotInstalled => "not_installed",
            Refusal::Present => "credential_present",
            Refusal::FolderInUse => "folder_in_use",
        }
    }
}

/// Why an operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ManagerError {
    Refused(Refusal),
    /// A step failed: its name and the failure's code. The operation was
    /// undone: an update's previous version runs again.
    Step {
        step: &'static str,
        code: &'static str,
    },
    /// A test's fault point: stopped as a crash would, nothing undone.
    #[cfg(any(test, feature = "testing"))]
    Crashed,
}

impl ManagerError {
    pub fn code(&self) -> &'static str {
        match self {
            ManagerError::Refused(r) => r.code(),
            ManagerError::Step { code, .. } => code,
            #[cfg(any(test, feature = "testing"))]
            ManagerError::Crashed => "crashed",
        }
    }
}

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManagerError::Step { step, code } => write!(f, "companion {step}: {code}"),
            other => write!(f, "companion: {}", other.code()),
        }
    }
}

impl std::error::Error for ManagerError {}

impl From<Refusal> for ManagerError {
    fn from(r: Refusal) -> ManagerError {
        ManagerError::Refused(r)
    }
}

fn failed(step: &'static str) -> impl Fn(&'static str) -> ManagerError {
    move |code| ManagerError::Step { step, code }
}

impl From<LayoutError> for ManagerError {
    fn from(e: LayoutError) -> ManagerError {
        failed("layout")(e.code())
    }
}

impl From<StateError> for ManagerError {
    fn from(e: StateError) -> ManagerError {
        failed("state")(e.code())
    }
}

/// What you confirm before an install or update: the entry, the pinned
/// version and its SHA-256, as shown ([`Offer`]). CMP-6 carries it from
/// the API; the CLI from `--sha256`. A bare "yes" can't make one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallToken {
    pub id: CompanionId,
    pub version: Version,
    pub sha256: Sha256,
}

/// What an install or update would put on disk, to show before you
/// confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub id: CompanionId,
    pub name: String,
    pub version: Version,
    pub sha256: Sha256,
    pub size: u64,
    pub licence: String,
    /// The installed version this replaces, for an update.
    pub replaces: Option<Version>,
}

/// What an uninstall deletes, exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// `<data>/companions/<id>`, with everything in it.
    pub folder: PathBuf,
    /// The `settings` rows of its credentials.
    pub rows: Vec<String>,
}

/// One companion in the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Present {
    pub id: CompanionId,
    pub version: Option<Version>,
    pub health: Health,
}

/// The last health the manager saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Unknown,
    Healthy,
    Unhealthy,
}

/// The installed companions for CMP-10's requirement checks. CMP-5g adds
/// detected ones; `download_clients` says whether any
/// `integrations.clients` entry exists (set by `crates/lodestar`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub companions: Vec<Present>,
    pub download_clients: bool,
}

/// Credential rows in `settings` (`companions.<id>.secret.<field>`),
/// implemented by `crates/lodestar` over the store.
pub trait Rows: Send + Sync {
    /// Stores a row durably (committed) before returning.
    fn put(&self, row: &SealedRow) -> Result<(), RowError>;
    fn get(&self, key: &str) -> Result<Option<SealedSecret>, RowError>;
    /// Deletes the companion's credential rows: one per
    /// [`Field`], exactly what [`Removal::rows`] lists.
    fn forget(&self, id: &CompanionId) -> Result<(), RowError>;
}

/// The store failed. Carries nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowError;

/// What [`Run::start`] starts.
pub struct Launch {
    pub id: CompanionId,
    pub executable: PathBuf,
    /// The installed version's folder.
    pub install: PathBuf,
    pub args: Vec<OsString>,
    pub target: Target,
    /// Redacted from its output.
    pub secrets: Vec<Secret>,
}

/// Starting, health and stopping (CMP-4a, CMP-4b); [`Supervised`] in
/// shipped builds.
pub trait Run: Send + Sync {
    /// Whether it can start anything at all, asked before an install
    /// begins so nothing is downloaded for a start that can't happen.
    fn ready(&self) -> Result<(), &'static str> {
        Ok(())
    }
    /// Starts it; a refusal's code otherwise.
    fn start(&self, launch: Launch) -> BoxFuture<'_, Result<(), &'static str>>;
    /// Whether it passes health before the implementation's deadline.
    fn healthy<'a>(&'a self, id: &'a CompanionId, target: &'a Target) -> BoxFuture<'a, bool>;
    /// Stops it, bounded; nothing if it isn't running.
    fn stop<'a>(&'a self, id: &'a CompanionId) -> BoxFuture<'a, ()>;
    /// Its live state while it's supervised, which the manager follows
    /// into the snapshot after each start it makes. `None`: not followed.
    fn status(&self, _id: &CompanionId) -> Option<watch::Receiver<Status>> {
        None
    }
}

/// Configuring an installed companion through its API (CMP-8b); [`Api`]
/// in shipped builds. Run after every start the manager makes, so a bind
/// or proxy changed behind Lodestar's back is put back.
pub trait ApiConfigure: Send + Sync {
    fn configure<'a>(
        &'a self,
        id: &'a CompanionId,
        target: &'a Target,
        receipt: InstallReceipt,
        credentials: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>>;

    /// Hands a companion Lodestar installed to Lodestar's own settings
    /// (CMP-8b's `hand_over`: qBittorrent as the managed download client)
    /// with its stored credential (X-cmp-native-handover). The manager
    /// calls it within an install, before its commit point, and within
    /// every restart and regenerate. Nothing to hand over by default.
    fn hand_over<'a>(
        &'a self,
        _id: &'a CompanionId,
        _target: &'a Target,
        _receipt: InstallReceipt,
        _credentials: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async { Ok(()) })
    }

    /// Whether an install of `id` (`fresh`: nothing installed yet) can be
    /// handed over, asked before anything is downloaded: the settings it
    /// goes to must be there, and a first install must not take over an
    /// entry that isn't Lodestar's. Yes by default.
    fn can_hand_over<'a>(
        &'a self,
        _id: &'a CompanionId,
        _fresh: bool,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async { Ok(()) })
    }

    /// Whether [`ApiConfigure::hand_over`] may save anything for `id`: a
    /// first install writes its marker only then. Yes by default, which
    /// fails safe: a marker for a companion that saves nothing only makes
    /// the next start release nothing (#692's review, B1).
    fn hands_over(&self, _id: &CompanionId) -> bool {
        true
    }

    /// Takes what [`ApiConfigure::hand_over`] saved out of Lodestar's
    /// settings again: before a removal, and after a first install that
    /// was rolled back. Nothing by default.
    fn release<'a>(&'a self, _id: &'a CompanionId) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Where downloads come from: the guarded client in shipped builds.
pub struct Downloader(Src);

enum Src {
    Guarded(Guarded),
    #[cfg(any(test, feature = "testing"))]
    Any(Arc<dyn fetch::Source>),
}

impl Downloader {
    pub fn new(src: Guarded) -> Downloader {
        Downloader(Src::Guarded(src))
    }

    /// Any source, for tests only.
    #[cfg(any(test, feature = "testing"))]
    pub fn any(src: Arc<dyn fetch::Source>) -> Downloader {
        Downloader(Src::Any(src))
    }

    async fn get(&self, data: &Path, entry: &Entry, pin: &Pin) -> Result<Verified, FetchError> {
        let mut progress = |_: u64| {};
        match &self.0 {
            Src::Guarded(g) => fetch::fetch(g, data, entry, pin, &mut progress).await,
            #[cfg(any(test, feature = "testing"))]
            Src::Any(s) => fetch::fetch_from(&**s, data, entry, pin, &mut progress).await,
        }
    }
}

/// Where a test stops the manager as a crash would.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Before the step's effect.
    Before(Step),
    /// After its effect, before the journal records it.
    After(Step),
    /// After the commit point, before the previous version is deleted.
    Prune,
}

/// What the manager is built from.
pub struct Parts {
    pub data: PathBuf,
    pub host: Host,
    pub catalogue: &'static Catalogue,
    pub downloader: Downloader,
    pub secrets: Arc<SecretStore>,
    pub rows: Arc<dyn Rows>,
    pub run: Arc<dyn Run>,
    pub api: Arc<dyn ApiConfigure>,
}

/// The manager. One per data folder; one operation per companion at a
/// time. A handle on shared state: the journal and disk work runs in
/// `spawn_blocking`, off the async runtime, on that state.
pub struct Manager(Arc<Inner>);

struct Inner {
    p: Parts,
    /// `<data>/companions/.lock`, held for the manager's life: one
    /// journal writer per data folder across processes (serve and the CLI).
    _lock: std::fs::File,
    busy: Mutex<Vec<CompanionId>>,
    snapshot: watch::Sender<Snapshot>,
    /// Each companion's run, counted up on every start and stop the
    /// manager makes: a follower publishes only for the run it follows.
    runs: Mutex<HashMap<CompanionId, u64>>,
    /// [`LIVE_DEADLINE`]; shorter only in tests.
    deadline: Mutex<Duration>,
    /// Companions whose first install a crash undid after its hand-over
    /// (their marker, [`Inner::marker`]): released once the settings can
    /// be reached ([`Manager::release_pending`]).
    pending: Mutex<Vec<CompanionId>>,
    #[cfg(any(test, feature = "testing"))]
    fault: Mutex<Option<Fault>>,
}

/// Holds a companion's operation slot.
struct Slot(Arc<Inner>, CompanionId);

impl Drop for Slot {
    fn drop(&mut self) {
        let mut b = self.0.busy.lock().unwrap_or_else(|e| e.into_inner());
        b.retain(|i| i != &self.1);
    }
}

/// How a companion is started: its arguments and its config file, under
/// its home folder.
struct Profile {
    args: Vec<OsString>,
    /// The config file's folders below home, and its name.
    config: (&'static [&'static str], &'static str),
}

fn profile(id: &CompanionId, home: &Path, port: u16) -> Option<Profile> {
    let flag = |k: &str| {
        let mut s = OsString::from(k);
        s.push(home.as_os_str());
        s
    };
    match id.as_str() {
        configure::qbittorrent::ID => Some(Profile {
            args: vec![flag("--profile="), format!("--webui-port={port}").into()],
            config: (&["qBittorrent", "config"], "qBittorrent.conf"),
        }),
        configure::prowlarr::ID => Some(Profile {
            args: vec!["-nobrowser".into(), flag("-data=")],
            config: (&[], "config.xml"),
        }),
        _ => None,
    }
}

/// The one credential each companion has: qBittorrent's WebUI password,
/// Prowlarr's `ApiKey`. `None` for a companion with none.
pub fn field_of(id: &CompanionId) -> Option<Field> {
    match id.as_str() {
        configure::qbittorrent::ID => Some(Field::WebUiPassword),
        configure::prowlarr::ID => Some(Field::ApiKey),
        _ => None,
    }
}

/// The program inside the version's folder: the catalogue's, or, for a
/// bare binary, the companion's id (`.exe` on Windows), the same for every
/// version so a rollback finds it.
fn executable(id: &CompanionId, a: &Artifact) -> Result<RelPath, ManagerError> {
    match &a.executable {
        Some(x) => Ok(x.clone()),
        None => {
            let ext = if a.platform == Platform::Windows {
                ".exe"
            } else {
                ""
            };
            RelPath::parse(&format!("{id}{ext}")).map_err(|_| failed("stage")("executable"))
        }
    }
}

/// Creates `home/<parts…>` one folder at a time, refusing a link or
/// anything but a directory at each step, so a config writer never
/// follows a link a companion left in its own home (#608's follow-up).
/// The companion is stopped while this runs.
fn config_dir(home: &Path, parts: &[&str]) -> Result<PathBuf, ManagerError> {
    let bad = failed("files");
    let mut dir = home.to_path_buf();
    for p in std::iter::once("").chain(parts.iter().copied()) {
        if !p.is_empty() {
            dir.push(p);
        }
        #[cfg_attr(windows, allow(unused_mut))] // only Unix sets a mode
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        match b.create(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(bad("io")),
            _ => {}
        }
        let ft = std::fs::symlink_metadata(&dir)
            .map_err(|_| bad("io"))?
            .file_type();
        if !ft.is_dir() || ft.is_symlink() {
            return Err(bad("config_path_link"));
        }
    }
    Ok(dir)
}

/// A companion's stored credential, opened on demand for CMP-8b.
struct Stored<'a> {
    m: &'a Inner,
    id: &'a CompanionId,
}

impl Credentials for Stored<'_> {
    fn get(&self, field: &str) -> Result<Option<Secret>, CredentialError> {
        let Some(f) = Field::ALL.into_iter().find(|f| f.as_str() == field) else {
            return Ok(None);
        };
        let key = CredentialKey::new(self.id.clone(), f);
        match self.m.p.rows.get(&key.row_key()) {
            Ok(Some(sealed)) => credentials::open(&self.m.p.secrets, &key, &sealed)
                .map(Some)
                .map_err(|_| CredentialError::Unavailable),
            Ok(None) => Ok(None),
            Err(RowError) => Err(CredentialError::Unavailable),
        }
    }
}

/// Whether a folder holds nothing but what a crash before `begun` can
/// leave: nothing at all, or the journal's own temporary file. An
/// unreadable folder doesn't qualify, so it's never deleted as ours.
fn leftovers_only(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut entries = entries;
    entries.all(|e| {
        e.is_ok_and(|e| {
            e.file_name() == state::STATE_TMP && e.file_type().is_ok_and(|t| t.is_file())
        })
    })
}

/// Deletes a folder [`leftovers_only`] accepted: the temporary file, then
/// the folder with `remove_dir`, which fails rather than delete anything
/// written there since the check.
fn remove_leftovers(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(dir.join(state::STATE_TMP)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::remove_dir(dir)
}

/// A credential opened before an API call, so the adapter's synchronous
/// `Credentials::get` doesn't touch the store on the async runtime.
struct Opened {
    field: &'static str,
    secret: Option<Secret>,
}

impl Credentials for Opened {
    fn get(&self, field: &str) -> Result<Option<Secret>, CredentialError> {
        Ok((field == self.field).then(|| self.secret.clone()).flatten())
    }
}

/// What recovery found that needs the async side.
enum Found {
    Done(Option<Version>),
    /// A confirmed removal to finish: stop it, then the rest.
    Remove(Record),
}

/// The synchronous core: the journal, the disk and the store. Every call
/// that does I/O runs inside [`Manager::off`], in `spawn_blocking`.
impl Inner {
    /// A test's fault point; nothing in other builds.
    #[cfg(any(test, feature = "testing"))]
    fn point(&self, here: Fault) -> Result<(), ManagerError> {
        let mut f = self.fault.lock().unwrap_or_else(|e| e.into_inner());
        if *f == Some(here) {
            *f = None;
            return Err(ManagerError::Crashed);
        }
        Ok(())
    }

    /// The companion's stored credential, opened.
    fn secret(&self, id: &CompanionId) -> Result<Option<Secret>, ManagerError> {
        Stored { m: self, id }
            .get(field_name(id))
            .map_err(|_| failed("credentials")("open"))
    }

    fn publish(&self, id: &CompanionId, now: Option<(Option<Version>, Health)>) {
        self.snapshot.send_modify(|s| {
            s.companions.retain(|c| &c.id != id);
            if let Some((version, health)) = now {
                s.companions.push(Present {
                    id: id.clone(),
                    version,
                    health,
                });
                s.companions
                    .sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
            }
        });
    }

    /// A new run of `id`: any follower of an earlier one stops publishing.
    fn next_run(&self, id: &CompanionId) -> u64 {
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        let n = runs.entry(id.clone()).or_default();
        *n += 1;
        *n
    }

    /// Publishes `health` for run `run` of `id`, unless a later start or
    /// stop has superseded it. Checked and published under one lock, so
    /// nothing a stop publishes is overwritten by an older run.
    fn publish_run(&self, id: &CompanionId, run: u64, version: &Version, health: Health) -> bool {
        let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        if runs.get(id) != Some(&run) {
            return false;
        }
        self.publish(id, Some((Some(version.clone()), health)));
        true
    }

    fn is_run(&self, id: &CompanionId, run: u64) -> bool {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).get(id) == Some(&run)
    }

    /// Ends run `run` of `id` if it's still the current one, as a stop
    /// does: `false` when a later run began meanwhile, which then owns it.
    fn end_run(&self, id: &CompanionId, run: u64) -> bool {
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        match runs.get_mut(id) {
            Some(n) if *n == run => {
                *n += 1;
                true
            }
            _ => false,
        }
    }

    /// The marker a first install's hand-over leaves while it may have
    /// saved and the install isn't `configured` yet:
    /// `<data>/companions/.handed-over-<id>`, beside the lock, outside the
    /// companion's own folder (which a rollback deletes only if it holds
    /// nothing else).
    fn marker(&self, id: &CompanionId) -> PathBuf {
        self.p
            .data
            .join("companions")
            .join(format!(".handed-over-{id}"))
    }

    /// Writes the marker durably before the hand-over is tried.
    fn mark(&self, id: &CompanionId) -> Result<(), ManagerError> {
        let path = self.marker(id);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        // Never through a link planted at its path.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let f = opts.open(&path).map_err(|_| failed("handover")("marker"))?;
        f.sync_all().map_err(|_| failed("handover")("marker"))?;
        #[cfg(unix)]
        if let Some(dir) = path.parent() {
            std::fs::File::open(dir)
                .and_then(|d| d.sync_all())
                .map_err(|_| failed("handover")("marker"))?;
        }
        Ok(())
    }

    /// Removes the marker: the install was configured, or what its
    /// hand-over saved was released. A failure leaves it for the next start,
    /// whose release finds nothing to take out.
    fn unmark(&self, id: &CompanionId) {
        let _ = std::fs::remove_file(self.marker(id));
    }

    fn marked(&self, id: &CompanionId) -> bool {
        std::fs::symlink_metadata(self.marker(id)).is_ok_and(|m| m.is_file())
    }

    /// How long a start, restart, regenerate or configuration may take.
    fn deadline(&self) -> Duration {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn claim(self: &Arc<Self>, id: &CompanionId) -> Result<Slot, Refusal> {
        let mut b = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        if b.contains(id) {
            return Err(Refusal::Busy);
        }
        b.push(id.clone());
        Ok(Slot(self.clone(), id.clone()))
    }

    /// The catalogue's own spelling of `id`, for log labels.
    fn label(&self, id: &CompanionId) -> &'static str {
        self.entry(id).map_or("unknown", |e| e.id.as_str())
    }

    fn entry(&self, id: &CompanionId) -> Result<&'static Entry, Refusal> {
        self.p.catalogue.get(id.as_str()).ok_or(Refusal::Unknown)
    }

    /// The entry's pinned build for this host, or why there's none.
    fn installable(
        &self,
        id: &CompanionId,
    ) -> Result<(&'static Entry, &'static Artifact, &'static Pin), Refusal> {
        let entry = self.entry(id)?;
        if self.p.host.in_container {
            return Err(Refusal::InContainer);
        }
        let (platform, arch) = self.p.host.platform.ok_or(Refusal::UnsupportedPlatform)?;
        // An installer is yours to run, pinned or not (owner cq4): say so
        // before the pin is looked at, so an unpinned one never reads as
        // "Lodestar will install this once pinned" (#705's review, I3).
        if entry
            .artifact(platform, arch)
            .is_some_and(|a| a.archive == ArchiveKind::Installer)
        {
            return Err(Refusal::GuidedOnly);
        }
        let (a, pin) = entry
            .installable(platform, arch)
            .map_err(Refusal::NotAvailable)?;
        if profile(id, Path::new(""), 0).is_none() || entry.health.is_none() {
            return Err(Refusal::NoProfile);
        }
        Ok((entry, a, pin))
    }

    fn target(entry: &Entry) -> Result<Target, ManagerError> {
        let h = entry.health.as_ref().ok_or(Refusal::NoProfile)?;
        let port = entry.port(h.port.as_str()).ok_or(Refusal::NoProfile)?;
        Ok(Target::Loopback(port.number))
    }

    /// The companion's folder, which must exist.
    fn folder(&self, id: &CompanionId) -> Result<Install, ManagerError> {
        Install::existing(&self.p.data, id)?.ok_or(failed("layout")("gone"))
    }

    fn record(&self, id: &CompanionId) -> Result<Option<Record>, ManagerError> {
        match Install::existing(&self.p.data, id)? {
            None => Ok(None),
            Some(i) => Ok(state::load(i.root())?),
        }
    }

    fn offer(&self, id: &CompanionId) -> Result<Offer, ManagerError> {
        let (entry, _, pin) = self.installable(id)?;
        let replaces = self.record(id)?.and_then(|r| r.installed);
        if replaces
            .as_ref()
            .is_some_and(|v| v.as_str() == pin.version.as_str())
        {
            return Err(Refusal::Current.into());
        }
        Ok(Offer {
            id: entry.id.clone(),
            name: entry.name.clone(),
            version: pin.version.clone(),
            sha256: pin.sha256,
            size: pin.size,
            licence: entry.licence.spdx.clone(),
            replaces,
        })
    }

    fn removal(&self, id: &CompanionId) -> Result<Removal, ManagerError> {
        let install = Install::existing(&self.p.data, id)?.ok_or(Refusal::NotInstalled)?;
        if state::load(install.root())?.is_none() {
            return Err(Refusal::NotInstalled.into());
        }
        Ok(Removal {
            folder: install.root().to_path_buf(),
            rows: Field::ALL
                .iter()
                .map(|f| CredentialKey::new(id.clone(), *f).row_key())
                .collect(),
        })
    }

    /// Records `step` and returns the record as written. The caller's copy
    /// changes only once the file holds it, so a failed write never reads
    /// as a step reached.
    fn save(&self, id: &CompanionId, rec: &Record, step: Step) -> Result<Record, ManagerError> {
        let mut next = rec.clone();
        if let Some(op) = next.op.as_mut() {
            op.step = step;
        }
        state::save(self.folder(id)?.root(), &next)?;
        Ok(next)
    }

    fn leftover(&self, id: &CompanionId, e: Option<LayoutError>) {
        if let Some(e) = e {
            lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = self.label(id),
                version = "",
                step = "leftover",
                reason = e.code()
            );
        }
    }

    /// Before anything changes: the checks an install makes under its
    /// slot, then the folder and the journal's `begun`.
    fn begin(&self, entry: &Entry, pin: &Pin) -> Result<Record, ManagerError> {
        let id = &entry.id;
        let existing = Install::existing(&self.p.data, id)?;
        let record = match &existing {
            Some(i) => state::load(i.root())?,
            None => None,
        };
        // A folder Lodestar has no record of isn't ours to install over or
        // to roll back to nothing: a compose companion's host folder can
        // sit there (CMP-7's validator allows it).
        if let (None, Some(i)) = (&record, &existing) {
            if !leftovers_only(i.root()) {
                return Err(Refusal::FolderInUse.into());
            }
            // An earlier first install that failed before `begun`.
            remove_leftovers(i.root()).map_err(|_| failed("layout")("leftover"))?;
        }
        let installed = match record {
            Some(r) if r.op.is_some() => return Err(Refusal::Busy.into()),
            Some(r) => r.installed,
            None => None,
        };
        if installed
            .as_ref()
            .is_some_and(|v| v.as_str() == pin.version.as_str())
        {
            return Err(Refusal::Current.into());
        }
        let key = field_of(id).map(|f| CredentialKey::new(id.clone(), f).row_key());
        if let (None, Some(k)) = (&installed, key) {
            let stored = self.p.rows.get(&k);
            if stored
                .map_err(|_| failed("credentials")("store"))?
                .is_some()
            {
                return Err(Refusal::Present.into());
            }
        }
        Install::open(&self.p.data, id)?;
        let kind = if installed.is_some() {
            OpKind::Update
        } else {
            OpKind::Install
        };
        let rec = Record {
            installed,
            op: Some(Op {
                kind,
                version: Some(pin.version.clone()),
                step: Step::Begun,
            }),
        };
        self.save(id, &rec, Step::Begun)
    }

    /// Step 1's disk half: stages the verified download.
    fn stage(
        &self,
        id: &CompanionId,
        artifact: &Artifact,
        pin: &Pin,
        mut file: Verified,
        exe: &RelPath,
    ) -> Result<(), ManagerError> {
        let mut install = self.folder(id)?;
        let r = match artifact.archive {
            ArchiveKind::Binary => install
                .stage_binary(&pin.version, &mut file, exe, pin.size)
                .map(drop),
            kind => install.stage(&pin.version, kind, &mut file, exe).map(drop),
        };
        let _ = file.discard();
        r.map_err(|e| failed("stage")(e.code()))
    }

    /// First install only: the credential row, stored durably before the
    /// config file uses it, then the file. Returns the secret.
    fn credentials_and_files(
        &self,
        id: &CompanionId,
        rec: &mut Record,
    ) -> Result<Secret, ManagerError> {
        let field = field_of(id).ok_or(Refusal::NoProfile)?;
        fault!(self, Before(Step::Credentials));
        let issued = credentials::generate(&self.p.secrets, &CredentialKey::new(id.clone(), field))
            .map_err(|_| failed("credentials")("generate"))?;
        self.p
            .rows
            .put(&issued.row)
            .map_err(|_| failed("credentials")("store"))?;
        fault!(self, After(Step::Credentials));
        *rec = self.save(id, rec, Step::Credentials)?;

        fault!(self, Before(Step::Files));
        self.write_credential(id, field, &issued.secret)?;
        fault!(self, After(Step::Files));
        *rec = self.save(id, rec, Step::Files)?;
        Ok(issued.secret)
    }

    /// Writes `secret` into the companion's own config file, keeping
    /// everything else in it (CMP-3's writers). The companion is stopped.
    fn write_credential(
        &self,
        id: &CompanionId,
        field: Field,
        secret: &Secret,
    ) -> Result<(), ManagerError> {
        let home = crate::supervise::process::home_dir(&self.p.data, id);
        let prof = profile(id, &home, 0).ok_or(Refusal::NoProfile)?;
        let dir = config_dir(&home, prof.config.0)?;
        let path = dir.join(prof.config.1);
        let written = match field {
            Field::WebUiPassword => {
                config_files::write_qbittorrent(&path, &Username::default(), secret)
            }
            Field::ApiKey => config_files::write_prowlarr(&path, secret),
        };
        written.map_err(|_| failed("files")("write"))
    }

    /// Regenerate's rotation (CMP-6): a new credential, never reading the
    /// old one's value. The row is stored first, then the file, as at
    /// install (#611): a crash between them leaves Lodestar holding the
    /// new value, and regenerating again writes both. A failed file write
    /// puts the old sealed row back, so the row and the file agree. The
    /// companion is stopped.
    fn rotate(&self, id: &CompanionId) -> Result<(), ManagerError> {
        let field = field_of(id).ok_or(Refusal::NoProfile)?;
        let key = CredentialKey::new(id.clone(), field);
        let store = failed("credentials");
        let old = self
            .p
            .rows
            .get(&key.row_key())
            .map_err(|_| store("store"))?;
        let issued = credentials::generate(&self.p.secrets, &key).map_err(|_| store("generate"))?;
        self.p.rows.put(&issued.row).map_err(|_| store("store"))?;
        let Err(e) = self.write_credential(id, field, &issued.secret) else {
            return Ok(());
        };
        let restored = match old {
            Some(value) => self.p.rows.put(&SealedRow {
                key: key.row_key(),
                value,
            }),
            None => self.p.rows.forget(id),
        };
        if restored.is_err() {
            lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = self.label(id),
                version = "",
                step = "regenerate",
                reason = "rollback_failed"
            );
        }
        Err(e)
    }

    /// Points `current` at the staged version, keeping the old one.
    fn commit(
        &self,
        id: &CompanionId,
        version: &Version,
        exe: &RelPath,
        rec: &mut Record,
    ) -> Result<(), ManagerError> {
        fault!(self, Before(Step::Committed));
        let sw = self.folder(id)?.commit(version, exe)?;
        self.leftover(id, sw.leftover);
        fault!(self, After(Step::Committed));
        *rec = self.save(id, rec, Step::Committed)?;
        Ok(())
    }

    /// Where `version`'s program and folder are.
    fn paths(
        &self,
        id: &CompanionId,
        version: &Version,
        exe: &RelPath,
    ) -> Result<(PathBuf, PathBuf), ManagerError> {
        let i = self.folder(id)?;
        Ok((i.executable(version, exe), i.version_dir(version)))
    }

    /// After the commit point: deletes the previous version and clears
    /// the operation.
    fn finish(&self, id: &CompanionId, rec: &mut Record) -> Result<(), ManagerError> {
        fault!(self, Prune);
        let mut install = self.folder(id)?;
        if install.pointer()?.is_some_and(|p| p.previous.is_some()) {
            let sw = install.prune_previous()?;
            self.leftover(id, sw.leftover);
        }
        let mut next = rec.clone();
        next.op = None;
        state::save(install.root(), &next)?;
        *rec = next;
        Ok(())
    }

    /// Rolls an operation that didn't reach its commit point back: an
    /// update to the version it replaces (returned), a first install to
    /// nothing (`None`, the folder and credential rows gone, the journal
    /// last).
    fn roll_back(
        &self,
        id: &CompanionId,
        rec: &mut Record,
    ) -> Result<Option<Version>, ManagerError> {
        let Some(op) = rec.op.clone() else {
            return Ok(rec.installed.clone());
        };
        let mut install = self.folder(id)?;
        match (op.kind, rec.installed.clone()) {
            (OpKind::Update, Some(prev)) => {
                let p = install.pointer()?;
                let moved = p.as_ref().is_some_and(|p| {
                    op.version
                        .as_ref()
                        .is_some_and(|v| v.as_str() == p.current.as_str())
                });
                if moved {
                    let sw = install.rollback()?;
                    self.leftover(id, sw.leftover);
                }
                install.sweep()?;
                let mut next = rec.clone();
                next.op = None;
                state::save(install.root(), &next)?;
                *rec = next;
                Ok(Some(prev))
            }
            _ => {
                self.p
                    .rows
                    .forget(id)
                    .map_err(|_| failed("credentials")("store"))?;
                // Only what Lodestar put there goes; the folder stays if
                // anything else is in it.
                install.remove_own(&["home", state::STATE_TMP], Some(state::STATE_FILE))?;
                Ok(None)
            }
        }
    }

    /// A removal's first step: what goes, and the journal's `begun`.
    fn begin_remove(&self, id: &CompanionId) -> Result<(Removal, Record), ManagerError> {
        let what = self.removal(id)?;
        let mut rec = self.record(id)?.unwrap_or_default();
        if rec.op.as_ref().is_some_and(|o| o.kind != OpKind::Remove) {
            return Err(Refusal::Busy.into());
        }
        rec.op = Some(Op {
            kind: OpKind::Remove,
            version: None,
            step: Step::Begun,
        });
        let rec = self.save(id, &rec, Step::Begun)?;
        Ok((what, rec))
    }

    /// A removal once the companion is stopped: credential rows, then the
    /// folder, the journal last, so a crash anywhere leaves a folder that
    /// still says it's being removed.
    fn end_remove(&self, id: &CompanionId, rec: &Record) -> Result<(), ManagerError> {
        let rec = self.save(id, rec, Step::Stopped)?;
        fault!(self, Before(Step::Forgotten));
        self.p
            .rows
            .forget(id)
            .map_err(|_| failed("credentials")("store"))?;
        self.save(id, &rec, Step::Forgotten)?;
        self.folder(id)?.remove_last(Some(state::STATE_FILE))?;
        self.publish(id, None);
        lodestar_log::log_event!(REMOVED, companion = self.label(id));
        Ok(())
    }

    /// Recovery for one companion, as far as it can go without stopping
    /// anything.
    fn recover_sync(&self, id: &CompanionId) -> Result<Found, ManagerError> {
        let Some(mut install) = Install::existing(&self.p.data, id)? else {
            return Ok(Found::Done(None));
        };
        let Some(mut rec) = state::load(install.root())? else {
            // No record. The only way Lodestar leaves a folder without one
            // is a crash (or a failed write) between creating it and
            // journalling `begun`, which leaves it empty or holding the
            // journal's temporary file: only then is it deleted. Anything
            // else (a compose companion's host folder, say) isn't ours.
            if leftovers_only(install.root()) && remove_leftovers(install.root()).is_ok() {
                lodestar_log::log_event!(RECOVERED, companion = self.label(id), step = "empty");
                return Ok(Found::Done(None));
            }
            return Err(failed("state")("not_ours"));
        };
        let Some(op) = rec.op.clone() else {
            install.sweep()?;
            return Ok(Found::Done(rec.installed));
        };
        let outcome = match op.kind {
            OpKind::Remove => return Ok(Found::Remove(rec)),
            _ if op.step >= Step::Configured => {
                self.finish(id, &mut rec)?;
                rec.installed
            }
            _ => self.roll_back(id, &mut rec)?,
        };
        lodestar_log::log_event!(RECOVERED, companion = self.label(id), step = op.step.code());
        Ok(Found::Done(outcome))
    }
}

/// How long a busy companion lock is tried again on Windows
/// (X-win-restart-followups-2, as `lodestar-search`'s index lock). Windows
/// releases a lock left on a closed file, or held by a process that ended,
/// "depending upon available system resources" (`LockFileEx`), so a quick
/// restart can find the previous run's lock still held for a moment. On
/// Unix a closed file's lock is gone at once, so a busy lock there is
/// another running process, refused at once.
#[cfg(windows)]
const LOCK_WAIT: Duration = Duration::from_secs(10);
#[cfg(not(windows))]
const LOCK_WAIT: Duration = Duration::ZERO;
/// How often it's tried meanwhile.
const LOCK_POLL: Duration = Duration::from_millis(100);

/// Calls `attempt` until it gives anything but a busy lock, or `wait` has
/// passed since the first call, sleeping `poll` between calls. Any other
/// error is returned at once. The clock and the sleep are passed in, so the
/// loop is tested on every platform without waiting. A `wait` of zero is
/// one attempt.
fn retry_busy<T>(
    mut attempt: impl FnMut() -> Result<T, std::fs::TryLockError>,
    wait: Duration,
    poll: Duration,
    now: impl Fn() -> std::time::Instant,
    mut sleep: impl FnMut(Duration),
) -> Result<T, std::fs::TryLockError> {
    let start = now();
    loop {
        match attempt() {
            Err(std::fs::TryLockError::WouldBlock)
                if now().saturating_duration_since(start) < wait =>
            {
                sleep(poll);
            }
            other => return other,
        }
    }
}

impl Manager {
    /// Takes the data folder's companion lock; [`Refusal::Busy`] while
    /// another process (serve, or a `lodestar companion` command) holds it.
    /// On Windows a busy lock is tried again for up to [`LOCK_WAIT`] first,
    /// for the previous run's lock after a quick restart.
    /// Blocking: call it from `spawn_blocking` in async code.
    pub fn open(parts: Parts) -> Result<Manager, ManagerError> {
        let dir = parts.data.join("companions");
        std::fs::create_dir_all(&dir).map_err(|_| failed("lock")("io"))?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))
            .map_err(|_| failed("lock")("io"))?;
        match retry_busy(
            || lock.try_lock(),
            LOCK_WAIT,
            LOCK_POLL,
            std::time::Instant::now,
            std::thread::sleep,
        ) {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(Refusal::Busy.into()),
            Err(std::fs::TryLockError::Error(_)) => return Err(failed("lock")("io")),
        }
        Ok(Manager(Arc::new(Inner {
            p: parts,
            _lock: lock,
            busy: Mutex::new(Vec::new()),
            snapshot: watch::channel(Snapshot::default()).0,
            runs: Mutex::new(HashMap::new()),
            deadline: Mutex::new(LIVE_DEADLINE),
            pending: Mutex::new(Vec::new()),
            #[cfg(any(test, feature = "testing"))]
            fault: Mutex::new(None),
        })))
    }

    /// Runs `f` on the shared state in `spawn_blocking`.
    async fn off<T, F>(&self, f: F) -> Result<T, ManagerError>
    where
        T: Send + 'static,
        F: FnOnce(&Inner) -> Result<T, ManagerError> + Send + 'static,
    {
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || f(&inner))
            .await
            .map_err(|_| failed("blocking")("panicked"))?
    }

    /// Shortens [`LIVE_DEADLINE`], so its expiry can be tested.
    #[cfg(any(test, feature = "testing"))]
    pub fn set_live_deadline(&self, d: Duration) {
        *self.0.deadline.lock().unwrap_or_else(|e| e.into_inner()) = d;
    }

    /// Stops the next operation at `fault`, as a crash would.
    #[cfg(any(test, feature = "testing"))]
    pub fn crash_at(&self, fault: Option<Fault>) {
        *self.0.fault.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    }

    /// The installed companions and their health (CMP-10).
    pub fn snapshot(&self) -> watch::Receiver<Snapshot> {
        self.0.snapshot.subscribe()
    }

    /// Whether any `integrations.clients` entry exists.
    pub fn set_download_clients(&self, any: bool) {
        self.0
            .snapshot
            .send_if_modified(|s| std::mem::replace(&mut s.download_clients, any) != any);
    }

    /// What installing or updating `id` would fetch, to show before you
    /// confirm. Reads the journal: blocking.
    pub fn offer(&self, id: &CompanionId) -> Result<Offer, ManagerError> {
        self.0.offer(id)
    }

    /// What removing `id` deletes. Reads the journal: blocking.
    pub fn removal(&self, id: &CompanionId) -> Result<Removal, ManagerError> {
        self.0.removal(id)
    }

    /// Installs or updates to the pinned version you confirmed.
    ///
    /// The operation runs in a task of its own: dropping this future
    /// doesn't stop it part-way, and its slot is held until it has finished
    /// (so two writers never share the journal's temporary file).
    pub async fn install(&self, token: &InstallToken) -> Result<Version, ManagerError> {
        let me = Manager(self.0.clone());
        let token = token.clone();
        tokio::spawn(async move { me.install_now(&token).await })
            .await
            .map_err(|_| failed("install")("panicked"))?
    }

    async fn install_now(&self, token: &InstallToken) -> Result<Version, ManagerError> {
        let m = &self.0;
        let (entry, artifact, pin) = m.installable(&token.id)?;
        if token.version.as_str() != pin.version.as_str() || token.sha256 != pin.sha256 {
            return Err(Refusal::Confirmation.into());
        }
        let id = &entry.id;
        m.p.run.ready().map_err(failed("start"))?;
        let _slot = m.claim(id)?;
        // Before anything is downloaded: a hand-over that can't happen
        // (no settings to go to, or an entry that isn't Lodestar's)
        // refuses now, rather than after the whole install.
        let fresh = self
            .off(move |m| m.record(id))
            .await?
            .is_none_or(|r| r.installed.is_none());
        m.p.api
            .can_hand_over(id, fresh)
            .await
            .map_err(|e| failed("handover")(configure::code(&e)))?;
        let mut rec = self.off(move |m| m.begin(entry, pin)).await?;
        let mut handed = false;
        match self
            .steps(entry, artifact, pin, &mut rec, &mut handed)
            .await
        {
            Ok(()) => {
                lodestar_log::log_event!(
                    INSTALLED,
                    companion = m.label(id),
                    version = pin.version.as_str()
                );
                Ok(pin.version.clone())
            }
            #[cfg(any(test, feature = "testing"))]
            Err(ManagerError::Crashed) => Err(ManagerError::Crashed),
            // Past the commit point it's installed: only deleting the
            // previous version failed, and the next recovery finishes it.
            Err(e) if rec.op.as_ref().is_some_and(|o| o.step >= Step::Configured) => {
                lodestar_log::log_event!(
                    INSTALL_FAILED,
                    companion = m.label(id),
                    version = pin.version.as_str(),
                    step = "leftover",
                    reason = e.code()
                );
                m.publish(id, Some((Some(pin.version.clone()), Health::Healthy)));
                self.follow(id, &pin.version);
                Ok(pin.version.clone())
            }
            Err(e) => {
                let (step, code) = match &e {
                    ManagerError::Step { step, code } => (*step, *code),
                    other => ("install", other.code()),
                };
                if !logged_at_bound(&e) {
                    lodestar_log::log_event!(
                        INSTALL_FAILED,
                        companion = m.label(id),
                        version = pin.version.as_str(),
                        step = step,
                        reason = code
                    );
                }
                self.undo(entry, rec, handed).await;
                Err(e)
            }
        }
    }

    async fn steps(
        &self,
        entry: &'static Entry,
        artifact: &'static Artifact,
        pin: &'static Pin,
        rec: &mut Record,
        handed: &mut bool,
    ) -> Result<(), ManagerError> {
        let m = &self.0;
        let id = &entry.id;
        let v = &pin.version;
        let fresh = rec.installed.is_none();
        let exe = executable(id, artifact)?;
        let target = Inner::target(entry)?;

        // Fetch, verify, extract.
        fault!(m, Before(Step::Staged));
        let file =
            m.p.downloader
                .get(&m.p.data, entry, pin)
                .await
                .map_err(|e| failed("fetch")(e.code()))?;
        let verify::Upstream::PinOnly = verify::upstream(pin);
        let x = exe.clone();
        let r = rec.clone();
        *rec = self
            .off(move |m| {
                m.stage(id, artifact, pin, file, &x)?;
                fault!(m, After(Step::Staged));
                m.save(id, &r, Step::Staged)
            })
            .await?;

        // Credentials and config file, first install only.
        let r = rec.clone();
        let (r, secrets) = self
            .off(move |m| {
                let mut r = r;
                let s = if fresh {
                    Some(m.credentials_and_files(id, &mut r)?)
                } else {
                    m.secret(id)?
                };
                Ok((r, s))
            })
            .await?;
        *rec = r;

        // Commit: the old version stops first; the pointer keeps it as
        // `previous`.
        self.halt(id).await;
        let (x, r) = (exe.clone(), rec.clone());
        *rec = self
            .off(move |m| {
                let mut r = r;
                m.commit(id, v, &x, &mut r)?;
                Ok(r)
            })
            .await?;

        // Start and health.
        fault!(m, Before(Step::Healthy));
        self.launch(entry, v, &exe, &target, secrets.into_iter().collect())
            .await?;
        fault!(m, After(Step::Healthy));
        let r = rec.clone();
        *rec = self.off(move |m| m.save(id, &r, Step::Healthy)).await?;

        // Configure through the API, with the receipt only this crate mints.
        fault!(m, Before(Step::Configured));
        let secret = self.off(move |m| m.secret(id)).await?;
        let opened = Opened {
            field: field_name(id),
            secret,
        };
        m.p.api
            .configure(id, &target, InstallReceipt::new(target.clone()), &opened)
            .await
            .map_err(|e| failed("configure")(configure::code(&e)))?;
        // Handed over before the commit point: a failed hand-over fails
        // the install, which is undone, so it's never "installed" without
        // being the download client (X-cmp-native-handover).
        // Counted before it's tried: one that saved and then failed (a
        // timeout after the save) is undone too. A first install found no
        // entry before it began (`can_hand_over`), so whatever is there
        // now is this install's.
        // A first install marks it first (#679's I2): a crash before
        // `configured` is undone by recovery, which can't reach the
        // settings, so the marker says what to release once it can.
        // Not for one whose hand-over saves nothing (`hands_over`, yes
        // unless an implementation says otherwise: a marker it didn't
        // need only makes the next start release nothing).
        let marks = fresh && m.p.api.hands_over(id);
        if marks {
            self.off(move |m| m.mark(id)).await?;
        }
        *handed = true;
        // One past its bound has settled by the time it answers
        // `timed_out` (`hand_over_now`), so the undo releases whatever it
        // saved, as for any other failure.
        self.hand_over_now(id, &target).await?;
        fault!(m, After(Step::Configured));
        let mut next = rec.clone();
        next.installed = Some(v.clone());
        *rec = self
            .off(move |m| m.save(id, &next, Step::Configured))
            .await?;
        if marks {
            self.off(move |m| {
                m.unmark(id);
                Ok(())
            })
            .await?;
        }

        // Done: the previous version goes.
        let r = rec.clone();
        *rec = self
            .off(move |m| {
                let mut r = r;
                m.finish(id, &mut r)?;
                Ok(r)
            })
            .await?;
        m.publish(id, Some((Some(v.clone()), Health::Healthy)));
        self.follow(id, v);
        Ok(())
    }

    /// Starts `version` and waits for health.
    async fn launch(
        &self,
        entry: &'static Entry,
        version: &Version,
        exe: &RelPath,
        target: &Target,
        secrets: Vec<Secret>,
    ) -> Result<(), ManagerError> {
        let m = &self.0;
        let id = &entry.id;
        let home = crate::supervise::process::home_dir(&m.p.data, id);
        let Target::Loopback(port) = target else {
            return Err(Refusal::NoProfile.into());
        };
        let prof = profile(id, &home, port.get()).ok_or(Refusal::NoProfile)?;
        let (v, x) = (version.clone(), exe.clone());
        let (executable, install) = self.off(move |m| m.paths(id, &v, &x)).await?;
        m.p.run
            .start(Launch {
                id: id.clone(),
                executable,
                install,
                args: prof.args,
                target: target.clone(),
                secrets,
            })
            .await
            .map_err(failed("start"))?;
        if !m.p.run.healthy(id, target).await {
            return Err(failed("health")("unhealthy"));
        }
        Ok(())
    }

    /// A live failure: rolls back as recovery would, then starts the
    /// previous version again after a failed update.
    /// `handed`: this install's hand-over saved, so a first install undone
    /// takes its entry out again; one that never got that far leaves the
    /// settings as they are (an entry there isn't this install's).
    async fn undo(&self, entry: &'static Entry, rec: Record, handed: bool) {
        let m = &self.0;
        let id = &entry.id;
        // Before `staged` the running version was never stopped.
        let touched = rec.op.as_ref().is_some_and(|o| o.step >= Step::Staged);
        if touched {
            self.halt(id).await;
        }
        let rolled = self
            .off(move |m| {
                let mut r = rec;
                m.roll_back(id, &mut r)
            })
            .await;
        match rolled {
            Ok(Some(_)) if !touched => {}
            Ok(Some(prev)) => {
                lodestar_log::log_event!(ROLLED_BACK, companion = m.label(id));
                let restarted = self.start_installed(entry, &prev).await;
                let health = if restarted.is_ok() {
                    Health::Healthy
                } else {
                    Health::Unhealthy
                };
                m.publish(id, Some((Some(prev.clone()), health)));
                self.follow_from(entry, &prev, health);
            }
            Ok(None) => {
                // A first install undone: what its hand-over saved goes
                // too, so no managed entry points at nothing.
                if handed {
                    match self.release_now(id).await {
                        Ok(()) => m.unmark(id),
                        // The marker stays: the next start releases it. One
                        // past its deadline was logged there.
                        Err(e) if logged_at_bound(&e) => {}
                        Err(_) => lodestar_log::log_event!(
                            INSTALL_FAILED,
                            companion = m.label(id),
                            version = "",
                            step = "release",
                            reason = "rollback_failed"
                        ),
                    }
                }
                m.publish(id, None)
            }
            Err(e) => lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = m.label(id),
                version = "",
                step = "rollback",
                reason = e.code()
            ),
        }
    }

    /// Starts an installed version and configures it (every start the
    /// manager makes re-asserts the bind and proxy, #608's follow-up).
    async fn start_installed(
        &self,
        entry: &'static Entry,
        version: &Version,
    ) -> Result<(), ManagerError> {
        let m = &self.0;
        let id = &entry.id;
        let (platform, arch) = m.p.host.platform.ok_or(Refusal::UnsupportedPlatform)?;
        let a = entry
            .artifact(platform, arch)
            .ok_or(Refusal::NotAvailable(NotAvailable::NoPlatform))?;
        let exe = executable(id, a)?;
        let target = Inner::target(entry)?;
        // The stored credential goes into its own file again first, so a
        // crash between a regenerate's row and its file heals at the next
        // start (the writers keep every other line). It's stopped.
        let secrets = self
            .off(move |m| {
                let secret = m.secret(id)?;
                if let (Some(s), Some(f)) = (&secret, field_of(id)) {
                    m.write_credential(id, f, s)?;
                }
                Ok(secret)
            })
            .await?;
        self.launch(entry, version, &exe, &target, secrets.into_iter().collect())
            .await?;
        self.configure_now(id, &target).await
    }

    /// CMP-8b's configuration of a running companion, with its stored
    /// credential: the bind and the proxy for its route, re-asserted.
    async fn configure_now(
        &self,
        id: &'static CompanionId,
        target: &Target,
    ) -> Result<(), ManagerError> {
        let secret = self.off(move |m| m.secret(id)).await?;
        let opened = Opened {
            field: field_name(id),
            secret,
        };
        let configured =
            self.0
                .p
                .api
                .configure(id, target, InstallReceipt::new(target.clone()), &opened);
        tokio::time::timeout(self.0.deadline(), configured)
            .await
            .map_err(|_| failed("configure")("timed_out"))?
            .map_err(|e| failed("configure")(configure::code(&e)))
    }

    /// Uninstalls `id`: stop, credential rows, then the folder. Like
    /// [`Manager::install`], it runs to completion in a task of its own.
    pub async fn remove(&self, id: &CompanionId) -> Result<Removal, ManagerError> {
        let me = Manager(self.0.clone());
        let id = id.clone();
        tokio::spawn(async move { me.remove_now(&id).await })
            .await
            .map_err(|_| failed("remove")("panicked"))?
    }

    async fn remove_now(&self, id: &CompanionId) -> Result<Removal, ManagerError> {
        let m = &self.0;
        let entry = m.entry(id)?;
        let id = &entry.id;
        let _slot = m.claim(id)?;
        // Only a companion Lodestar installed (`not_installed` otherwise)
        // has its managed entry taken out, and that first: a failure
        // leaves everything as it was, to try again.
        self.off(move |m| m.removal(id)).await?;
        // An install or update a crash left in its journal is recovery's:
        // refused before anything is released. A removal it left is taken
        // up again (its entry went before it began).
        if self
            .off(move |m| m.record(id))
            .await?
            .and_then(|r| r.op)
            .is_some_and(|o| o.kind != OpKind::Remove)
        {
            return Err(Refusal::Busy.into());
        }
        self.release_now(id).await?;
        let (what, rec) = self.off(move |m| m.begin_remove(id)).await?;
        self.finish_remove(entry, rec).await?;
        Ok(what)
    }

    async fn finish_remove(&self, entry: &'static Entry, rec: Record) -> Result<(), ManagerError> {
        let id = &entry.id;
        fault!(self.0, Before(Step::Stopped));
        self.halt(id).await;
        self.off(move |m| m.end_remove(id, &rec)).await
    }

    /// Finishes or undoes what a crash interrupted, for every catalogue
    /// companion with a folder, before anything starts. Returns what each
    /// one ended as (`None`: nothing installed). Inside a container nothing
    /// is native (Q17), so nothing is looked at.
    pub async fn recover(&self) -> Vec<(CompanionId, Result<Option<Version>, ManagerError>)> {
        let m = &self.0;
        let mut out = Vec::new();
        if m.p.host.in_container {
            return out;
        }
        for entry in m.p.catalogue.entries() {
            let r = self.recover_one(entry).await;
            if let Ok(v) = &r {
                m.publish(&entry.id, v.clone().map(|v| (Some(v), Health::Unknown)));
                // A hand-over a crash interrupted: still installed, its
                // entry is right; undone, it's released later.
                let id = &entry.id;
                if m.marked(id) {
                    if v.is_some() {
                        m.unmark(id);
                    } else {
                        m.pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(id.clone());
                    }
                }
            }
            out.push((entry.id.clone(), r));
        }
        out
    }

    /// Forgets what a crash left pending for `id`, and its marker: a guided
    /// confirmation is handing `id` over, so the entry is yours from now
    /// on, and a release at the next start must never take it out.
    /// Blocking.
    pub fn clear_handover(&self, id: &CompanionId) {
        self.0
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|p| p != id);
        self.0.unmark(id);
    }

    /// Releases what a crash's undone first installs had handed over
    /// (#679's I2), once the settings can be reached: serve calls it when
    /// the sink is wired. Each is released under its slot and only while
    /// nothing is installed; one that fails keeps its marker, for the
    /// next start.
    pub async fn release_pending(&self) {
        let pending =
            std::mem::take(&mut *self.0.pending.lock().unwrap_or_else(|e| e.into_inner()));
        for id in pending {
            let Ok(entry) = self.0.entry(&id) else {
                continue;
            };
            let id = &entry.id;
            let Ok(_slot) = self.0.claim(id) else {
                continue;
            };
            let installed = self
                .off(move |m| m.record(id))
                .await
                .map(|r| r.is_some_and(|r| r.installed.is_some()));
            if !matches!(installed, Ok(false)) {
                continue;
            }
            match self.release_now(id).await {
                Ok(()) => self.0.unmark(id),
                // Logged already, at the deadline.
                Err(e) if logged_at_bound(&e) => {}
                Err(e) => lodestar_log::log_event!(
                    INSTALL_FAILED,
                    companion = self.0.label(id),
                    version = "",
                    step = "release",
                    reason = e.code()
                ),
            }
        }
    }

    async fn recover_one(&self, entry: &'static Entry) -> Result<Option<Version>, ManagerError> {
        let m = &self.0;
        let id = &entry.id;
        let _slot = m.claim(id)?;
        match self.off(move |m| m.recover_sync(id)).await? {
            Found::Done(v) => Ok(v),
            Found::Remove(rec) => {
                let step = rec.op.as_ref().map_or(Step::Begun, |o| o.step);
                self.finish_remove(entry, rec).await?;
                lodestar_log::log_event!(RECOVERED, companion = m.label(id), step = step.code());
                Ok(None)
            }
        }
    }
}

/// A hand-over or release past its bound, which `hand_over_now` and
/// `release_now` log there: their callers don't log it again.
fn logged_at_bound(e: &ManagerError) -> bool {
    matches!(
        e,
        ManagerError::Step {
            step: "handover" | "release",
            code: "timed_out"
        }
    )
}

/// What the manager does to a companion it installed, once installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveOp {
    /// Serve's start of an installed companion.
    Start,
    /// Stop, then start and configure again, on the route as set now.
    Restart,
    /// Stop, rotate its credential, start and configure again.
    Regenerate,
}

impl LiveOp {
    fn step(self) -> &'static str {
        match self {
            LiveOp::Start => "start",
            LiveOp::Restart => "restart",
            LiveOp::Regenerate => "regenerate",
        }
    }
}

/// How long a start, restart or regenerate may take to start the
/// companion and configure it: the health wait ([`HEALTH_DEADLINE`]) and
/// the configuration's requests, with room to spare. The stop before it
/// is bounded by the supervisor.
pub const LIVE_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// How often a follower that has to configure its companion looks for the
/// slot while an operation holds it.
const SLOT_POLL: Duration = Duration::from_millis(250);

/// A restart or regenerate under way, in a task of its own that holds the
/// companion's slot until it's done (as [`Manager::install`]). Dropping it
/// leaves the operation running.
#[derive(Debug)]
pub struct Pending(JoinHandle<Result<(), ManagerError>>);

impl Pending {
    /// Waits for the operation to end.
    pub async fn finished(self) -> Result<(), ManagerError> {
        self.0.await.map_err(|_| failed("live")("panicked"))?
    }
}

impl Manager {
    /// Starts a companion Lodestar installed and configures it (serve,
    /// after recovery). Refused while another operation on it runs
    /// ([`Refusal::Busy`]), and for one it didn't install.
    pub async fn start(&self, id: &CompanionId) -> Result<(), ManagerError> {
        self.begin(id, LiveOp::Start).await?.finished().await
    }

    /// Restarts it: stops it, then starts and configures it again on its
    /// route as set now. Refused during an install or any other operation
    /// on it (`busy`), and for one Lodestar didn't install.
    pub async fn restart(&self, id: &CompanionId) -> Result<Pending, ManagerError> {
        self.begin(id, LiveOp::Restart).await
    }

    /// Replaces its credential with a new random one: stops it, writes the
    /// new one into its row and its own config file ([`Inner::rotate`]),
    /// then starts and configures it. Only for a companion Lodestar
    /// installed: one you added isn't Lodestar's to rotate.
    pub async fn regenerate(&self, id: &CompanionId) -> Result<Pending, ManagerError> {
        self.begin(id, LiveOp::Regenerate).await
    }

    /// Claims the slot and checks the journal, then runs `op` in a task
    /// of its own that keeps the slot.
    async fn begin(&self, id: &CompanionId, op: LiveOp) -> Result<Pending, ManagerError> {
        let m = &self.0;
        let entry = m.entry(id)?;
        if m.p.host.in_container {
            return Err(Refusal::NotInstalled.into());
        }
        let id = &entry.id;
        let slot = m.claim(id)?;
        let version = match self.off(move |m| m.record(id)).await? {
            Some(Record { op: Some(_), .. }) => return Err(Refusal::Busy.into()),
            Some(Record {
                installed: Some(v), ..
            }) => v,
            _ => return Err(Refusal::NotInstalled.into()),
        };
        let me = Manager(self.0.clone());
        Ok(Pending(tokio::spawn(async move {
            let _slot = slot;
            me.live(entry, &version, op).await
        })))
    }

    async fn live(
        &self,
        entry: &'static Entry,
        version: &Version,
        op: LiveOp,
    ) -> Result<(), ManagerError> {
        let m = &self.0;
        let id = &entry.id;
        match op {
            // Already running (an operation started it meanwhile): left
            // as it is, with its follower.
            LiveOp::Start if m.p.run.status(id).is_some() => return Ok(()),
            LiveOp::Start => {}
            _ => self.halt(id).await,
        }
        let rotated = match op {
            LiveOp::Regenerate => self.off(move |m| m.rotate(id)).await,
            _ => Ok(()),
        };
        // A restart or regenerate hands it over again with the credential
        // stored now, whether or not it then starts: Integrations and the
        // companion's own file always hold the same one. A restart is the
        // retry for a hand-over that failed.
        let handed = match op {
            LiveOp::Start => Ok(()),
            _ => match Inner::target(entry) {
                Ok(t) => self.hand_over_now(id, &t).await,
                Err(e) => Err(e),
            },
        };
        // Started again whatever the rotation did: a failed one put the
        // old credential back.
        let started = tokio::time::timeout(self.0.deadline(), self.start_installed(entry, version))
            .await
            .unwrap_or_else(|_| Err(failed(op.step())("timed_out")));
        // Its health is the start's: a failed rotation still started it,
        // on the old credential.
        let health = if started.is_ok() {
            Health::Healthy
        } else {
            Health::Unhealthy
        };
        let result = rotated.and(handed).and(started);
        m.publish(id, Some((Some(version.clone()), health)));
        // Followed even after a failed health check: a companion held for
        // its route is still supervised, and starts when the route is up.
        self.follow_from(entry, version, health);
        if let Err(e) = &result
            && e.code() != "shutting_down"
            && !logged_at_bound(e)
        {
            lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = m.label(id),
                version = "",
                step = op.step(),
                reason = e.code()
            );
        }
        result
    }

    /// [`ApiConfigure::release`], bounded like every settings write. One
    /// that passes the deadline is logged and then waited for: it may still
    /// save, and whoever releases holds the slot and, for an install's undo
    /// or a pending release, the guided lock, so no guided confirmation
    /// can save an entry that it then takes out (#692's review,
    /// Important 3). It still answers `timed_out`, so the marker stays and
    /// the next start releases whatever is left.
    async fn release_now(&self, id: &CompanionId) -> Result<(), ManagerError> {
        let mut release = std::pin::pin!(self.0.p.api.release(id));
        match tokio::time::timeout(self.0.deadline(), release.as_mut()).await {
            Ok(r) => r.map_err(|e| failed("release")(configure::code(&e))),
            Err(_) => {
                lodestar_log::log_event!(
                    INSTALL_FAILED,
                    companion = self.0.label(id),
                    version = "",
                    step = "release",
                    reason = "timed_out"
                );
                let _ = release.await;
                Err(failed("release")("timed_out"))
            }
        }
    }

    /// [`ApiConfigure::hand_over`] with the credential stored now.
    async fn hand_over_now(
        &self,
        id: &'static CompanionId,
        target: &Target,
    ) -> Result<(), ManagerError> {
        let secret = self.off(move |m| m.secret(id)).await?;
        let opened = Opened {
            field: field_name(id),
            secret,
        };
        // A generous bound, three of the live deadline (15 min in shipped
        // builds): one local settings write takes far less. One past it is
        // logged and then waited for, as a release is (`release_now`): it
        // may still save, and the caller holds the slot and, for an
        // install, the guided lock, so no guided confirmation can save an
        // entry that it then overwrites (#692's review, N1). It still
        // answers `timed_out`, once it has settled.
        let handed =
            self.0
                .p
                .api
                .hand_over(id, target, InstallReceipt::new(target.clone()), &opened);
        let mut handed = std::pin::pin!(handed);
        match tokio::time::timeout(3 * self.0.deadline(), handed.as_mut()).await {
            Ok(r) => r.map_err(|e| failed("handover")(configure::code(&e))),
            Err(_) => {
                lodestar_log::log_event!(
                    INSTALL_FAILED,
                    companion = self.0.label(id),
                    version = "",
                    step = "handover",
                    reason = "timed_out"
                );
                let _ = handed.await;
                Err(failed("handover")("timed_out"))
            }
        }
    }

    /// Stops `id`, ending the run any follower publishes for.
    async fn halt(&self, id: &CompanionId) {
        self.0.next_run(id);
        self.0.p.run.stop(id).await;
    }

    /// [`Manager::follow_from`] a healthy start.
    fn follow(&self, id: &CompanionId, version: &Version) {
        if let Ok(entry) = self.0.entry(id) {
            self.follow_from(entry, version, Health::Healthy);
        }
    }

    /// Follows the supervisor's state for `id` into the snapshot, for this
    /// run only, from `now` (what was just published). Backing off, held
    /// for its route, needing attention or stopped is unhealthy. Running
    /// again after any of those is healthy once it passes a health check
    /// ([`Run::healthy`]): the supervisor says running as soon as it has
    /// started it. A start, or failed checks the supervisor hasn't acted
    /// on yet, change nothing: it restarts a companion after Q7's three,
    /// which then shows. It ends when the companion stops being supervised
    /// or a later run begins.
    fn follow_from(&self, entry: &'static Entry, version: &Version, now: Health) {
        let m = &self.0;
        let id = &entry.id;
        let run = m.next_run(id);
        let Some(mut status) = m.p.run.status(id) else {
            return;
        };
        let Ok(target) = Inner::target(entry) else {
            return;
        };
        let weak = Arc::downgrade(&self.0);
        let check = m.p.run.clone();
        let version = version.clone();
        tokio::spawn(async move {
            let mut last = now;
            // A start that passed health was configured; one that didn't
            // (held for its route, say) is configured once it runs.
            let mut configured = now == Health::Healthy;
            loop {
                let state = status.borrow_and_update().state;
                let next = match state {
                    State::Running if last != Health::Healthy => {
                        if !check.healthy(id, &target).await {
                            Some(Health::Unhealthy)
                        } else if configured {
                            Some(Health::Healthy)
                        } else {
                            // Under the slot, and only for the current
                            // run (#660's L1): an operation that holds the
                            // slot is waited for, never left to leave it
                            // running unconfigured. One that starts a new
                            // run takes it over; past the deadline it's
                            // stopped. The slot is kept until any stop.
                            let (me, slot, outcome) = match slot_for(&weak, id, run).await {
                                Ok(Some((m, slot))) => (Manager(m), Some(slot), Ok(())),
                                Ok(None) => return,
                                Err(e) => match weak.upgrade() {
                                    Some(m) => (Manager(m), None, Err(e)),
                                    None => return,
                                },
                            };
                            // It may have stopped running while it waited
                            // (held for its route): then it's configured
                            // when it runs again, never while it's down.
                            let outcome = match outcome {
                                Ok(()) if status.borrow().state != State::Running => None,
                                Ok(()) => Some(me.configure_now(id, &target).await),
                                Err(e) => Some(Err(e)),
                            };
                            match outcome {
                                None => None,
                                Some(Ok(())) => {
                                    configured = true;
                                    Some(Health::Healthy)
                                }
                                Some(Err(e)) => {
                                    // Q8: never left running unconfigured,
                                    // where its bind or proxy may have
                                    // drifted.
                                    me.0.publish_run(id, run, &version, Health::Unhealthy);
                                    lodestar_log::log_event!(
                                        INSTALL_FAILED,
                                        companion = me.0.label(id),
                                        version = "",
                                        step = "configure",
                                        reason = e.code()
                                    );
                                    // Only this run: one an operation
                                    // began meanwhile is left alone.
                                    if me.0.end_run(id, run) {
                                        me.0.p.run.stop(id).await;
                                    }
                                    drop(slot);
                                    return;
                                }
                            }
                        }
                    }
                    State::Running | State::Starting | State::Unhealthy { .. } => None,
                    State::BackingOff { .. }
                    | State::NeedsAttention(_)
                    | State::Held
                    | State::Stopped => Some(Health::Unhealthy),
                };
                if let Some(h) = next.filter(|h| *h != last) {
                    let Some(m) = weak.upgrade() else {
                        return;
                    };
                    if !m.publish_run(id, run, &version, h) {
                        return;
                    }
                    last = h;
                }
                if status.changed().await.is_err() {
                    return;
                }
            }
        });
    }
}

/// The companion's slot for run `run`, waited for while an operation
/// holds it, with the manager it belongs to. `None` once a later run has
/// begun (that operation owns the companion now) or the manager is gone;
/// `busy` if it's still held after the deadline. Holds the manager only
/// for each look, never while it waits.
async fn slot_for(
    weak: &std::sync::Weak<Inner>,
    id: &CompanionId,
    run: u64,
) -> Result<Option<(Arc<Inner>, Slot)>, ManagerError> {
    let Some(deadline) = weak.upgrade().map(|m| m.deadline()) else {
        return Ok(None);
    };
    let give_up = tokio::time::Instant::now() + deadline;
    loop {
        let Some(m) = weak.upgrade() else {
            return Ok(None);
        };
        if !m.is_run(id, run) {
            return Ok(None);
        }
        if let Ok(slot) = m.claim(id) {
            // Checked again under the slot: nothing can begin a run now.
            return Ok(m.is_run(id, run).then_some((m, slot)));
        }
        if tokio::time::Instant::now() >= give_up {
            return Err(Refusal::Busy.into());
        }
        drop(m);
        tokio::time::sleep(SLOT_POLL).await;
    }
}

fn field_name(id: &CompanionId) -> &'static str {
    field_of(id).map_or("", Field::as_str)
}

/// A companion's egress route in CMP-8b's closed form, if it has one.
pub type ConfigRoute = dyn Fn(&CompanionId) -> Option<configure::Route> + Send + Sync;

/// The real [`ApiConfigure`]: CMP-8b's adapter for the companion, through
/// the guarded client. `route` is the companion's own egress route, as
/// CMP-8b's closed type (CMP-5g supplies it from Q18's setting).
pub struct Api {
    pub http: Arc<dyn GuardedHttp>,
    pub route: Arc<ConfigRoute>,
}

impl ApiConfigure for Api {
    fn configure<'a>(
        &'a self,
        id: &'a CompanionId,
        target: &'a Target,
        receipt: InstallReceipt,
        credentials: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async move {
            let Some(c) = configure::configurator(id) else {
                return Ok(());
            };
            let mut input = crate::adapter::ConfigureInput::new(credentials)
                .installed(receipt)
                .webui(configure::WebUiBind::Loopback);
            if let Some(r) = (self.route)(id) {
                input = input.route(r);
            }
            let plan = c.plan(&*self.http, target, &input).await?;
            c.apply(&*self.http, target, &input, &plan).await
        })
    }
}

/// How long [`Supervised::healthy`] waits for a first healthy answer.
pub const HEALTH_DEADLINE: Duration = Duration::from_secs(120);

/// Each companion's route (Q18), built afresh for every start from its
/// `companions.<id>.egress` setting, with [`monitored`] (`crates/lodestar`
/// implements it). `None`: there's no route it may follow, and it isn't
/// started (Q8: never on a route nobody chose).
pub trait Routes: Send + Sync {
    fn route<'a>(&'a self, id: &'a CompanionId) -> BoxFuture<'a, Option<Prelaunch>>;
}

/// A start's route: its live state for the gate, and what the companion
/// is launched with so it follows the route from its first instruction
/// (X-cmp-egress-prelaunch), before CMP-8b configures it.
#[derive(Debug, Clone)]
pub struct Prelaunch {
    pub route: Route,
    /// The SOCKS5 proxy of a SOCKS5 route, when its address is known.
    pub proxy: Option<configure::Socks5Proxy>,
}

/// The proxy variables every HTTP stack a companion uses reads at start
/// (.NET's `HttpClient.DefaultProxy`, which Prowlarr's clients use while
/// its own proxy setting is empty, on Linux and Windows alike), each set to
/// the same value. Upper case is read everywhere; on Unix the lower case
/// too, which .NET's Unix reader checks first, so the result doesn't
/// depend on the order (runner, 2026-11-11). Windows' names aren't case
/// sensitive, so one of each.
#[cfg(unix)]
const PROXY_VARS: [&str; 6] = [
    "ALL_PROXY",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "all_proxy",
    "http_proxy",
    "https_proxy",
];
#[cfg(not(unix))]
const PROXY_VARS: [&str; 3] = ["ALL_PROXY", "HTTP_PROXY", "HTTPS_PROXY"];

/// The bypass variable's names, as [`PROXY_VARS`].
#[cfg(unix)]
const NO_PROXY_VARS: [&str; 2] = ["NO_PROXY", "no_proxy"];
#[cfg(not(unix))]
const NO_PROXY_VARS: [&str; 1] = ["NO_PROXY"];

/// The oldest .NET whose `DefaultProxy` takes `socks5://` from the
/// variables: .NET 6 and 7 read only `http://` there, so a `socks5://`
/// value doesn't parse and everything goes direct (dotnet/runtime
/// `release/6.0`, `release/7.0` and `release/8.0`,
/// `HttpEnvironmentProxy.GetUriFromString`).
const DOTNET_SOCKS_ENV: u64 = 8;

/// The largest `runtimeconfig.json` read.
const RUNTIMECONFIG_MAX: u64 = 64 * 1024;

/// Whether the program reads [`launch_env`]'s proxy variables: it's a
/// .NET program whose `<name>.runtimeconfig.json`, beside it, names .NET
/// [`DOTNET_SOCKS_ENV`] or later (`runtimeOptions.tfm`, and every
/// `Microsoft.NETCore.App` framework it lists or includes, as a
/// self-contained build does). Anything else, unreadable or missing
/// included, doesn't, so its SOCKS5 route can't be launched on.
pub fn reads_proxy_env(executable: &Path) -> bool {
    let stem = match executable.extension() {
        Some(e) if e.eq_ignore_ascii_case("exe") => executable.with_extension(""),
        _ => executable.to_path_buf(),
    };
    let mut name = stem.into_os_string();
    name.push(".runtimeconfig.json");
    let path = PathBuf::from(name);
    // Opened once, then checked and read through the same handle, so what
    // is read is what was checked; never more than the cap (#672's review).
    // Never through a link, and never waiting on a FIFO (#679's review).
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
        );
    }
    // On Windows, the link itself is opened (`FILE_FLAG_OPEN_REPARSE_POINT`)
    // and then refused by its attribute, as `O_NOFOLLOW` refuses it on Unix
    // (#692's review).
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let Ok(file) = opts.open(&path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    if !meta.is_file() || meta.len() > RUNTIMECONFIG_MAX {
        return false;
    }
    let mut text = String::new();
    let read = std::io::Read::read_to_string(
        &mut std::io::Read::take(file, RUNTIMECONFIG_MAX + 1),
        &mut text,
    );
    if read.is_err() || text.len() as u64 > RUNTIMECONFIG_MAX {
        return false;
    }
    runtime_reads_proxy_env(&text)
}

/// [`reads_proxy_env`]'s check of the file's text.
fn runtime_reads_proxy_env(text: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let opts = &v["runtimeOptions"];
    let major = |s: &str| -> Option<u64> { s.split('.').next()?.parse().ok() };
    let tfm = opts["tfm"]
        .as_str()
        .and_then(|t| t.strip_prefix("net"))
        .and_then(major);
    let mut frameworks = Vec::new();
    for key in ["framework", "frameworks", "includedFrameworks"] {
        match &opts[key] {
            serde_json::Value::Array(a) => frameworks.extend(a.iter()),
            f @ serde_json::Value::Object(_) => frameworks.push(f),
            _ => {}
        }
    }
    let core: Vec<Option<u64>> = frameworks
        .iter()
        .filter(|f| f["name"] == "Microsoft.NETCore.App")
        .map(|f| f["version"].as_str().and_then(major))
        .collect();
    let new_enough = |m: Option<u64>| m.is_some_and(|m| m >= DOTNET_SOCKS_ENV);
    new_enough(tfm) && !core.is_empty() && core.into_iter().all(new_enough)
}

/// Loopback only, which never leaves the machine: a companion's own WebUI
/// and other companions on this machine.
/// `[::1]` as well as `::1`: .NET 8 compares entries with the URL's host,
/// which keeps IPv6 brackets.
const NO_PROXY: &str = "localhost,127.0.0.1,::1,[::1]";

/// The route in the companion's launch environment, or `None` when it
/// can't be expressed there, and the companion mustn't start: Q8's "never
/// direct, then fixed". Direct needs nothing. A SOCKS5 route is the proxy
/// variables (`socks5://`, so names are resolved by the proxy). Interface
/// and bind-address routes have no launch form yet: no companion setting
/// can choose them (sources offer direct and SOCKS5 only).
pub fn launch_env(p: &Prelaunch) -> Option<Vec<(EnvName, OsString)>> {
    match (p.route.kind, &p.proxy) {
        (RouteKind::Direct, _) => Some(Vec::new()),
        (RouteKind::Socks5, Some(proxy)) => {
            let url = OsString::from(format!("socks5://{}", proxy.get()));
            let proxies = PROXY_VARS.iter().map(|v| (*v, url.clone()));
            let bypass = NO_PROXY_VARS.iter().map(|v| (*v, OsString::from(NO_PROXY)));
            proxies
                .chain(bypass)
                .map(|(k, v)| Some((EnvName::parse(k).ok()?, v)))
                .collect()
        }
        _ => None,
    }
}

/// The real [`Run`]: CMP-4a's supervisor inside CMP-4b's sandbox, each
/// companion behind its route's gate.
pub struct Supervised {
    pub data: PathBuf,
    pub supervisor: tokio::sync::Mutex<Supervisor>,
    pub sandbox: Sandbox,
    pub http: Arc<dyn GuardedHttp>,
    /// Each companion's route (Q18).
    pub routes: Arc<dyn Routes>,
    /// Where qBittorrent saves, if set.
    pub downloads: Option<PathBuf>,
    live: Mutex<HashMap<CompanionId, Running>>,
    /// Set by [`Supervised::shutdown`]: nothing starts after it.
    closed: std::sync::atomic::AtomicBool,
}

/// What [`Supervised`] keeps for a companion it started.
struct Running {
    /// Dropped before the companion is removed, which holds it first.
    gate: Option<Gated>,
    /// Dropped at its stop, so `monitored`'s relay stops probing.
    route: Option<watch::Receiver<RouteState>>,
    status: Option<watch::Receiver<Status>>,
    /// Its output, kept after it stops until it's started again.
    logs: Option<LogRing>,
    /// The route variables it was launched with ([`launch_env`]), read
    /// back by tests ([`Supervised::launched_env`]).
    #[cfg_attr(not(any(test, feature = "testing")), allow(dead_code))]
    env: Vec<(EnvName, OsString)>,
}

impl Supervised {
    pub fn new(
        data: PathBuf,
        supervisor: Supervisor,
        sandbox: Sandbox,
        http: Arc<dyn GuardedHttp>,
        routes: Arc<dyn Routes>,
        downloads: Option<PathBuf>,
    ) -> Supervised {
        Supervised {
            data,
            supervisor: tokio::sync::Mutex::new(supervisor),
            sandbox,
            http,
            routes,
            downloads,
            live: Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<CompanionId, Running>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Its recent output, as the ring holds it (redacted with its
    /// credentials before each line went in). `None`: it was never started
    /// in this run of Lodestar.
    pub fn logs(&self, id: &CompanionId) -> Option<Vec<String>> {
        self.running()
            .get(id)
            .and_then(|r| r.logs.as_ref())
            .map(LogRing::lines)
    }

    /// The route variables it was last launched with: none on a direct
    /// route, the proxy on a SOCKS5 one. No secret is among them. For tests.
    #[cfg(any(test, feature = "testing"))]
    pub fn launched_env(&self, id: &CompanionId) -> Option<Vec<(String, OsString)>> {
        self.running().get(id).map(|r| {
            r.env
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.clone()))
                .collect()
        })
    }

    /// Stops every companion, each bounded as [`Run::stop`], and starts
    /// nothing after. For serve's shutdown.
    pub async fn shutdown(&self) {
        let mut sup = self.supervisor.lock().await;
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let ids: Vec<CompanionId> = self.running().keys().cloned().collect();
        for id in ids {
            let gate = self.running().get_mut(&id).and_then(|r| {
                r.status = None;
                r.route = None;
                r.gate.take()
            });
            drop(gate);
            sup.remove(&id).await;
        }
    }
}

impl Run for Supervised {
    fn start(&self, l: Launch) -> BoxFuture<'_, Result<(), &'static str>> {
        Box::pin(async move {
            let health = crate::detect::health(&l.id).ok_or("no_health")?;
            let pre = self.routes.route(&l.id).await.ok_or("no_route")?;
            // A route it can't follow at all is refused as before; one it
            // could follow but can't be launched on, likewise.
            egress::decide(Follows::of(&l.id), pre.route.kind).map_err(|e| e.code())?;
            let env = launch_env(&pre).ok_or("route_not_expressible")?;
            // Only a .NET that parses `socks5://` reads the variables:
            // an older one ignores them and goes direct.
            if !env.is_empty() {
                // A file read: off the async runtime. A failed task fails
                // closed like any other unreadable file.
                let exe = l.executable.clone();
                let reads = tokio::task::spawn_blocking(move || reads_proxy_env(&exe))
                    .await
                    .unwrap_or(false);
                if !reads {
                    return Err("route_not_expressible");
                }
            }
            let route = pre.route;
            let spec = SpawnSpec {
                id: l.id.clone(),
                executable: l.executable,
                args: l.args,
                env,
                data_dir: self.data.clone(),
                allow_privileged: false,
            };
            let conf = Confinement {
                install: l.install,
                downloads: self.downloads.clone(),
            };
            let (spec, _report) = self.sandbox.prepare(spec, &conf).map_err(|e| e.code())?;
            // What it's really launched with, as the sandbox passes it on.
            let env: Vec<(EnvName, OsString)> = spec
                .env
                .iter()
                .filter(|(k, _)| {
                    PROXY_VARS.contains(&k.as_str()) || NO_PROXY_VARS.contains(&k.as_str())
                })
                .cloned()
                .collect();
            let c = Companion {
                spawn: spec,
                depends_on: Vec::new(),
                probe: Some(Probe {
                    health: Arc::from(health),
                    http: self.http.clone(),
                    target: l.target,
                }),
                secrets: l.secrets,
                stop: None,
            };
            let mut sup = self.supervisor.lock().await;
            // Under the lock shutdown holds.
            if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("shutting_down");
            }
            let state = route.state.clone();
            let gated =
                egress::add(&mut sup, c, Follows::of(&l.id), route).map_err(|e| e.code())?;
            let running = Running {
                gate: Some(gated),
                route: Some(state),
                status: sup.status(&l.id),
                logs: sup.logs(&l.id),
                env,
            };
            self.running().insert(l.id, running);
            Ok(())
        })
    }

    /// Fails fast while its route is down (#621 N3): a held companion
    /// isn't running, so waiting the whole deadline tells nothing. A route
    /// not checked yet is waited for.
    fn healthy<'a>(&'a self, id: &'a CompanionId, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let Some(h) = crate::detect::health(id) else {
                return false;
            };
            let route = self.running().get(id).and_then(|r| r.route.clone());
            let deadline = tokio::time::Instant::now() + HEALTH_DEADLINE;
            while tokio::time::Instant::now() < deadline {
                if route
                    .as_ref()
                    .is_some_and(|r| matches!(*r.borrow(), RouteState::Down(_)))
                {
                    return false;
                }
                if h.check(&*self.http, target).await == HealthStatus::Healthy {
                    return true;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            false
        })
    }

    fn stop<'a>(&'a self, id: &'a CompanionId) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // Dropping the gate holds it first, so it can't be released
            // while it's removed. Its logs stay until it starts again.
            let gate = self.running().get_mut(id).and_then(|r| {
                r.status = None;
                r.route = None;
                r.gate.take()
            });
            drop(gate);
            self.supervisor.lock().await.remove(id).await;
        })
    }

    fn status(&self, id: &CompanionId) -> Option<watch::Receiver<Status>> {
        self.running().get(id).and_then(|r| r.status.clone())
    }
}

/// A route whose state turns down when its monitor stops (#608's
/// follow-up): `PluginEgress`'s own channel keeps its last value while
/// the egress lives, so a dead monitor would leave a stale `Up` releasing
/// the companion. The relay runs the monitor and forwards its states;
/// when the monitor returns or the task is aborted or panics, the relay's
/// channel says down (or closes, which the gate also reads as down).
pub fn monitored(egress: Arc<PluginEgress>, every: Duration) -> (Route, JoinHandle<()>) {
    let kind = egress.status().route;
    let mut src = egress.watch();
    let (tx, rx) = watch::channel(RouteState::Unchecked);
    let task = tokio::spawn(async move {
        let monitor = egress.monitor(every);
        tokio::pin!(monitor);
        loop {
            let _ = tx.send(*src.borrow_and_update());
            tokio::select! {
                () = &mut monitor => break,
                r = src.changed() => if r.is_err() { break },
                // Nobody reads the route any more: stop probing it.
                () = tx.closed() => return,
            }
        }
        let _ = tx.send(RouteState::Down(DownReason::ClientUnavailable));
    });
    (Route { kind, state: rx }, task)
}

macro_rules! opaque_debug {
    ($($t:ty),*) => {$(
        impl std::fmt::Debug for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($t)).finish_non_exhaustive()
            }
        }
    )*};
}

opaque_debug!(Downloader, Parts, Manager, Api, Supervised);

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launch")
            .field("id", &self.id)
            .field("executable", &self.executable)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs::TryLockError;
    use std::time::Instant;

    /// The busy-lock retry (X-win-restart-followups-2), on a fake clock
    /// that each sleep moves on. Returns the result and the attempts made.
    fn retried(
        answers: Vec<Result<u8, TryLockError>>,
        wait: Duration,
    ) -> (Result<u8, TryLockError>, usize) {
        let start = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let attempts = Cell::new(0usize);
        let mut answers = answers.into_iter();
        let got = retry_busy(
            || {
                attempts.set(attempts.get() + 1);
                answers.next().unwrap_or(Err(TryLockError::WouldBlock))
            },
            wait,
            LOCK_POLL,
            || start + elapsed.get(),
            |d| elapsed.set(elapsed.get() + d),
        );
        (got, attempts.get())
    }

    #[test]
    fn a_busy_lock_that_frees_is_taken() {
        let (got, attempts) = retried(
            vec![Err(TryLockError::WouldBlock), Ok(7)],
            Duration::from_secs(10),
        );
        assert_eq!((got.ok(), attempts), (Some(7), 2));
    }

    #[test]
    fn a_lock_busy_past_the_wait_is_refused() {
        // Tried at 0, 100, …, 1,000 ms: at the deadline it's given up.
        let (got, attempts) = retried(Vec::new(), Duration::from_secs(1));
        assert!(matches!(got, Err(TryLockError::WouldBlock)));
        assert_eq!(attempts, 11);
        // No wait (Unix): refused at once.
        let (got, attempts) = retried(Vec::new(), Duration::ZERO);
        assert!(matches!(got, Err(TryLockError::WouldBlock)));
        assert_eq!(attempts, 1);
    }

    /// Unix waits for nothing: a busy lock there is another process.
    #[cfg(not(windows))]
    #[test]
    fn unix_never_waits_for_the_lock() {
        assert_eq!(LOCK_WAIT, Duration::ZERO);
    }

    #[test]
    fn a_lock_error_is_never_retried() {
        let (got, attempts) = retried(
            vec![
                Err(TryLockError::Error(std::io::Error::other("denied"))),
                Ok(1),
            ],
            Duration::from_secs(10),
        );
        assert!(matches!(got, Err(TryLockError::Error(_))));
        assert_eq!(attempts, 1);
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::runtime_reads_proxy_env as reads;

    /// #679's review: the runtime config is never read through a link at
    /// its own path, and a FIFO there is refused at once (it isn't a
    /// regular file) instead of blocking.
    #[cfg(unix)]
    #[test]
    fn a_linked_or_fifo_runtime_config_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Prowlarr");
        let config = dir.path().join("Prowlarr.runtimeconfig.json");
        let good = r#"{"runtimeOptions":{"tfm":"net8.0","includedFrameworks":[{"name":"Microsoft.NETCore.App","version":"8.0.11"}]}}"#;
        std::fs::write(&config, good).unwrap();
        assert!(super::reads_proxy_env(&exe));
        let real = dir.path().join("elsewhere.json");
        std::fs::rename(&config, &real).unwrap();
        std::os::unix::fs::symlink(&real, &config).unwrap();
        assert!(!super::reads_proxy_env(&exe));
        std::fs::remove_file(&config).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&config)
            .status()
            .unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let exe2 = exe.clone();
        std::thread::spawn(move || {
            let _ = tx.send(super::reads_proxy_env(&exe2));
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(10));
        assert_eq!(got, Ok(false), "a FIFO blocked the read");
    }

    #[test]
    fn only_dotnet_8_or_later_reads_the_proxy_variables() {
        let framework_dependent = |tfm: &str, v: &str| {
            format!(
                r#"{{"runtimeOptions":{{"tfm":"{tfm}","framework":{{"name":"Microsoft.NETCore.App","version":"{v}"}}}}}}"#
            )
        };
        let self_contained = |tfm: &str, v: &str| {
            format!(
                r#"{{"runtimeOptions":{{"tfm":"{tfm}","includedFrameworks":[{{"name":"Microsoft.NETCore.App","version":"{v}"}},{{"name":"Microsoft.AspNetCore.App","version":"{v}"}}]}}}}"#
            )
        };
        for ok in [
            framework_dependent("net8.0", "8.0.11"),
            self_contained("net8.0", "8.0.11"),
            self_contained("net10.0", "10.0.0"),
        ] {
            assert!(reads(&ok), "{ok}");
        }
        for bad in [
            framework_dependent("net6.0", "6.0.36"),
            self_contained("net7.0", "7.0.20"),
            // A framework older than its tfm says, or no runtime named.
            self_contained("net8.0", "6.0.36"),
            r#"{"runtimeOptions":{"tfm":"net8.0"}}"#.to_owned(),
            r#"{"runtimeOptions":{"tfm":"netcoreapp3.1"}}"#.to_owned(),
            r#"{"runtimeOptions":{}}"#.to_owned(),
            "not json".to_owned(),
        ] {
            assert!(!reads(&bad), "{bad}");
        }
    }
}
