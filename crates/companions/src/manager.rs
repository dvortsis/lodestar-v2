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
use lodestar_sources::egress::{DownReason, PluginEgress, RouteState};
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
use crate::supervise::{Companion, Confinement, Probe, Sandbox, SpawnSpec, Supervisor};
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
/// time.
pub struct Manager {
    p: Parts,
    /// `<data>/companions/.lock`, held for the manager's life: one
    /// journal writer per data folder across processes (serve and the CLI).
    _lock: std::fs::File,
    busy: Mutex<Vec<CompanionId>>,
    snapshot: watch::Sender<Snapshot>,
    #[cfg(any(test, feature = "testing"))]
    fault: Mutex<Option<Fault>>,
}

/// Holds a companion's operation slot.
struct Slot<'a>(&'a Manager, CompanionId);

impl Drop for Slot<'_> {
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

fn field_of(id: &CompanionId) -> Option<Field> {
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
    m: &'a Manager,
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

impl Manager {
    /// Takes the data folder's companion lock; [`Refusal::Busy`] while
    /// another process (serve, or a `lodestar companion` command) holds it.
    pub fn open(parts: Parts) -> Result<Manager, ManagerError> {
        let dir = parts.data.join("companions");
        std::fs::create_dir_all(&dir).map_err(|_| failed("lock")("io"))?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))
            .map_err(|_| failed("lock")("io"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(Refusal::Busy.into()),
            Err(std::fs::TryLockError::Error(_)) => return Err(failed("lock")("io")),
        }
        Ok(Manager {
            p: parts,
            _lock: lock,
            busy: Mutex::new(Vec::new()),
            snapshot: watch::channel(Snapshot::default()).0,
            #[cfg(any(test, feature = "testing"))]
            fault: Mutex::new(None),
        })
    }

    /// Stops the next operation at `fault`, as a crash would.
    #[cfg(any(test, feature = "testing"))]
    pub fn crash_at(&self, fault: Option<Fault>) {
        *self.fault.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    }

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

    /// The installed companions and their health (CMP-10).
    pub fn snapshot(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.subscribe()
    }

    /// Whether any `integrations.clients` entry exists.
    pub fn set_download_clients(&self, any: bool) {
        self.snapshot
            .send_if_modified(|s| std::mem::replace(&mut s.download_clients, any) != any);
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

    fn claim(&self, id: &CompanionId) -> Result<Slot<'_>, Refusal> {
        let mut b = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        if b.contains(id) {
            return Err(Refusal::Busy);
        }
        b.push(id.clone());
        Ok(Slot(self, id.clone()))
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
        let (a, pin) = entry
            .installable(platform, arch)
            .map_err(Refusal::NotAvailable)?;
        if a.archive == ArchiveKind::Installer {
            return Err(Refusal::GuidedOnly);
        }
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

    fn record(&self, id: &CompanionId) -> Result<Option<Record>, ManagerError> {
        match Install::existing(&self.p.data, id)? {
            None => Ok(None),
            Some(i) => Ok(state::load(i.root())?),
        }
    }

    /// What installing or updating `id` would fetch, to show before you
    /// confirm.
    pub fn offer(&self, id: &CompanionId) -> Result<Offer, ManagerError> {
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

    /// What removing `id` deletes.
    pub fn removal(&self, id: &CompanionId) -> Result<Removal, ManagerError> {
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

    /// Records `step`; the in-memory record changes only once it's on
    /// disk, so a failed write never reads as a step reached.
    fn save(&self, install: &Install, rec: &mut Record, step: Step) -> Result<(), ManagerError> {
        let mut next = rec.clone();
        if let Some(op) = next.op.as_mut() {
            op.step = step;
        }
        state::save(install.root(), &next)?;
        *rec = next;
        Ok(())
    }

    /// Installs or updates to the pinned version you confirmed.
    pub async fn install(&self, token: &InstallToken) -> Result<Version, ManagerError> {
        let (entry, artifact, pin) = self.installable(&token.id)?;
        if token.version.as_str() != pin.version.as_str() || token.sha256 != pin.sha256 {
            return Err(Refusal::Confirmation.into());
        }
        let id = &entry.id;
        self.p.run.ready().map_err(failed("start"))?;
        let _slot = self.claim(id)?;
        if self.record(id)?.is_none() {
            // A folder with a pointer but no record isn't ours to install
            // over: recovery leaves it alone too.
            let folder = Install::existing(&self.p.data, id)?;
            if folder.map(|i| i.pointer()).transpose()?.flatten().is_some() {
                return Err(failed("state")("no_record"));
            }
        }
        let installed = match self.record(id)? {
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
        let mut install = Install::open(&self.p.data, id)?;
        let kind = if installed.is_some() {
            OpKind::Update
        } else {
            OpKind::Install
        };
        let mut rec = Record {
            installed,
            op: Some(Op {
                kind,
                version: Some(pin.version.clone()),
                step: Step::Begun,
            }),
        };
        self.save(&install, &mut rec, Step::Begun)?;
        match self
            .steps(entry, artifact, pin, &mut install, &mut rec)
            .await
        {
            Ok(()) => {
                lodestar_log::log_event!(
                    INSTALLED,
                    companion = self.label(id),
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
                    companion = self.label(id),
                    version = pin.version.as_str(),
                    step = "leftover",
                    reason = e.code()
                );
                self.publish(id, Some((Some(pin.version.clone()), Health::Healthy)));
                Ok(pin.version.clone())
            }
            Err(e) => {
                let (step, code) = match &e {
                    ManagerError::Step { step, code } => (*step, *code),
                    other => ("install", other.code()),
                };
                lodestar_log::log_event!(
                    INSTALL_FAILED,
                    companion = self.label(id),
                    version = pin.version.as_str(),
                    step = step,
                    reason = code
                );
                self.undo(entry, install, rec).await;
                Err(e)
            }
        }
    }

    async fn steps(
        &self,
        entry: &'static Entry,
        artifact: &'static Artifact,
        pin: &'static Pin,
        install: &mut Install,
        rec: &mut Record,
    ) -> Result<(), ManagerError> {
        let id = &entry.id;
        let v = &pin.version;
        let fresh = rec.installed.is_none();
        let exe = executable(id, artifact)?;
        let target = Self::target(entry)?;
        let home = crate::supervise::process::home_dir(&self.p.data, id);

        // Fetch, verify, extract.
        fault!(self, Before(Step::Staged));
        let mut file = self
            .p
            .downloader
            .get(&self.p.data, entry, pin)
            .await
            .map_err(|e| failed("fetch")(e.code()))?;
        let verify::Upstream::PinOnly = verify::upstream(pin);
        let staged = {
            let (kind, version, exe) = (artifact.archive, v.clone(), exe.clone());
            let mut owned = Install::existing(&self.p.data, id)?.ok_or(failed("stage")("gone"))?;
            tokio::task::spawn_blocking(move || {
                let r = match kind {
                    ArchiveKind::Binary => owned
                        .stage_binary(&version, &mut file, &exe, pin.size)
                        .map(drop),
                    _ => owned.stage(&version, kind, &mut file, &exe).map(drop),
                };
                let _ = file.discard();
                r
            })
            .await
            .map_err(|_| failed("stage")("panicked"))?
        };
        staged.map_err(|e| failed("stage")(e.code()))?;
        fault!(self, After(Step::Staged));
        self.save(install, rec, Step::Staged)?;

        // Credentials and config file, first install only. The row is
        // stored durably before anything uses the secret.
        let mut secrets = Vec::new();
        if fresh {
            let field = field_of(id).ok_or(Refusal::NoProfile)?;
            fault!(self, Before(Step::Credentials));
            let issued =
                credentials::generate(&self.p.secrets, &CredentialKey::new(id.clone(), field))
                    .map_err(|_| failed("credentials")("generate"))?;
            self.p
                .rows
                .put(&issued.row)
                .map_err(|_| failed("credentials")("store"))?;
            fault!(self, After(Step::Credentials));
            self.save(install, rec, Step::Credentials)?;

            fault!(self, Before(Step::Files));
            let prof = profile(id, &home, 0).ok_or(Refusal::NoProfile)?;
            let dir = config_dir(&home, prof.config.0)?;
            let path = dir.join(prof.config.1);
            let written = match field {
                Field::WebUiPassword => {
                    config_files::write_qbittorrent(&path, &Username::default(), &issued.secret)
                }
                Field::ApiKey => config_files::write_prowlarr(&path, &issued.secret),
            };
            written.map_err(|_| failed("files")("write"))?;
            fault!(self, After(Step::Files));
            self.save(install, rec, Step::Files)?;
            secrets.push(issued.secret);
        } else {
            secrets.extend(self.secret(id)?);
        }

        // Commit: the old version stops first; the pointer keeps it as
        // `previous`.
        self.p.run.stop(id).await;
        fault!(self, Before(Step::Committed));
        let sw = install.commit(v, &exe)?;
        self.leftover(id, sw.leftover);
        fault!(self, After(Step::Committed));
        self.save(install, rec, Step::Committed)?;

        // Start and health.
        fault!(self, Before(Step::Healthy));
        self.launch(entry, install, v, &exe, &target, secrets)
            .await?;
        fault!(self, After(Step::Healthy));
        self.save(install, rec, Step::Healthy)?;

        // Configure through the API, with the receipt only this crate mints.
        fault!(self, Before(Step::Configured));
        self.p
            .api
            .configure(
                id,
                &target,
                InstallReceipt::new(target.clone()),
                &Stored { m: self, id },
            )
            .await
            .map_err(|e| failed("configure")(configure::code(&e)))?;
        fault!(self, After(Step::Configured));
        let mut next = rec.clone();
        next.installed = Some(v.clone());
        self.save(install, &mut next, Step::Configured)?;
        *rec = next;

        // Done: the previous version goes.
        self.finish(id, install, rec)?;
        self.publish(id, Some((Some(v.clone()), Health::Healthy)));
        Ok(())
    }

    /// Starts `version` and waits for health.
    async fn launch(
        &self,
        entry: &Entry,
        install: &Install,
        version: &Version,
        exe: &RelPath,
        target: &Target,
        secrets: Vec<Secret>,
    ) -> Result<(), ManagerError> {
        let id = &entry.id;
        let home = crate::supervise::process::home_dir(&self.p.data, id);
        let Target::Loopback(port) = target else {
            return Err(Refusal::NoProfile.into());
        };
        let prof = profile(id, &home, port.get()).ok_or(Refusal::NoProfile)?;
        self.p
            .run
            .start(Launch {
                id: id.clone(),
                executable: install.executable(version, exe),
                install: install.version_dir(version),
                args: prof.args,
                target: target.clone(),
                secrets,
            })
            .await
            .map_err(failed("start"))?;
        if !self.p.run.healthy(id, target).await {
            return Err(failed("health")("unhealthy"));
        }
        Ok(())
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

    /// After the commit point: deletes the previous version and clears
    /// the operation.
    fn finish(
        &self,
        id: &CompanionId,
        install: &mut Install,
        rec: &mut Record,
    ) -> Result<(), ManagerError> {
        fault!(self, Prune);
        if install.pointer()?.is_some_and(|p| p.previous.is_some()) {
            let sw = install.prune_previous()?;
            self.leftover(id, sw.leftover);
        }
        rec.op = None;
        state::save(install.root(), rec)?;
        Ok(())
    }

    /// A live failure: rolls back as recovery would, then starts the
    /// previous version again after a failed update.
    async fn undo(&self, entry: &Entry, mut install: Install, mut rec: Record) {
        let id = &entry.id;
        // Before `staged` the running version was never stopped.
        let touched = rec.op.as_ref().is_some_and(|o| o.step >= Step::Staged);
        if touched {
            self.p.run.stop(id).await;
        }
        match self.roll_back(id, &mut install, &mut rec) {
            Ok(Some(_)) if !touched => {}
            Ok(Some(prev)) => {
                lodestar_log::log_event!(ROLLED_BACK, companion = self.label(id));
                let restarted = self.start_installed(entry, &install, &prev).await;
                let health = if restarted.is_ok() {
                    Health::Healthy
                } else {
                    Health::Unhealthy
                };
                self.publish(id, Some((Some(prev), health)));
            }
            Ok(None) => self.publish(id, None),
            Err(e) => lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = self.label(id),
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
        entry: &Entry,
        install: &Install,
        version: &Version,
    ) -> Result<(), ManagerError> {
        let id = &entry.id;
        let (platform, arch) = self.p.host.platform.ok_or(Refusal::UnsupportedPlatform)?;
        let a = entry
            .artifact(platform, arch)
            .ok_or(Refusal::NotAvailable(NotAvailable::NoPlatform))?;
        let exe = executable(id, a)?;
        let target = Self::target(entry)?;
        let secrets = self.secret(id)?.into_iter().collect();
        self.launch(entry, install, version, &exe, &target, secrets)
            .await?;
        self.p
            .api
            .configure(
                id,
                &target,
                InstallReceipt::new(target.clone()),
                &Stored { m: self, id },
            )
            .await
            .map_err(|e| failed("configure")(configure::code(&e)))
    }

    /// Rolls an operation that didn't reach its commit point back: an
    /// update to the version it replaces (returned), a first install to
    /// nothing (`None`, the folder and credential rows gone).
    fn roll_back(
        &self,
        id: &CompanionId,
        install: &mut Install,
        rec: &mut Record,
    ) -> Result<Option<Version>, ManagerError> {
        let Some(op) = rec.op.clone() else {
            return Ok(rec.installed.clone());
        };
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
                rec.op = None;
                state::save(install.root(), rec)?;
                Ok(Some(prev))
            }
            _ => {
                self.p
                    .rows
                    .forget(id)
                    .map_err(|_| failed("credentials")("store"))?;
                let i = Install::existing(&self.p.data, id)?.ok_or(failed("layout")("gone"))?;
                i.remove()?;
                Ok(None)
            }
        }
    }

    /// Uninstalls `id`: stop, credential rows, then the folder.
    pub async fn remove(&self, id: &CompanionId) -> Result<Removal, ManagerError> {
        self.entry(id)?;
        let _slot = self.claim(id)?;
        let what = self.removal(id)?;
        let install = Install::existing(&self.p.data, id)?.ok_or(Refusal::NotInstalled)?;
        let mut rec = state::load(install.root())?.unwrap_or_default();
        if rec.op.as_ref().is_some_and(|o| o.kind != OpKind::Remove) {
            return Err(Refusal::Busy.into());
        }
        rec.op = Some(Op {
            kind: OpKind::Remove,
            version: None,
            step: Step::Begun,
        });
        self.save(&install, &mut rec, Step::Begun)?;
        self.finish_remove(id, install, rec).await?;
        Ok(what)
    }

    async fn finish_remove(
        &self,
        id: &CompanionId,
        install: Install,
        mut rec: Record,
    ) -> Result<(), ManagerError> {
        fault!(self, Before(Step::Stopped));
        self.p.run.stop(id).await;
        self.save(&install, &mut rec, Step::Stopped)?;
        fault!(self, Before(Step::Forgotten));
        self.p
            .rows
            .forget(id)
            .map_err(|_| failed("credentials")("store"))?;
        self.save(&install, &mut rec, Step::Forgotten)?;
        install.remove()?;
        self.publish(id, None);
        lodestar_log::log_event!(REMOVED, companion = self.label(id));
        Ok(())
    }

    /// Finishes or undoes what a crash interrupted, for every catalogue
    /// companion with a folder, before anything starts. Returns what each
    /// one ended as (`None`: nothing installed).
    pub async fn recover(&self) -> Vec<(CompanionId, Result<Option<Version>, ManagerError>)> {
        let mut out = Vec::new();
        for entry in self.p.catalogue.entries() {
            let r = self.recover_one(&entry.id).await;
            if let Ok(v) = &r {
                self.publish(&entry.id, v.clone().map(|v| (Some(v), Health::Unknown)));
            }
            out.push((entry.id.clone(), r));
        }
        out
    }

    async fn recover_one(&self, id: &CompanionId) -> Result<Option<Version>, ManagerError> {
        let _slot = self.claim(id)?;
        let Some(mut install) = Install::existing(&self.p.data, id)? else {
            return Ok(None);
        };
        let Some(mut rec) = state::load(install.root())? else {
            // No record: a first install that crashed before its journal,
            // or a removal that had deleted it. Without a pointer nothing
            // ran; with one, it isn't ours to touch.
            if install.pointer()?.is_some() {
                return Err(failed("state")("no_record"));
            }
            // Its credential rows aren't touched: neither path left any of
            // ours, and a detected companion's must survive.
            install.remove()?;
            lodestar_log::log_event!(RECOVERED, companion = self.label(id), step = "no_record");
            return Ok(None);
        };
        let Some(op) = rec.op.clone() else {
            install.sweep()?;
            return Ok(rec.installed);
        };
        let outcome = match op.kind {
            OpKind::Remove => {
                self.finish_remove(id, install, rec).await?;
                None
            }
            _ if op.step >= Step::Configured => {
                self.finish(id, &mut install, &mut rec)?;
                rec.installed
            }
            _ => self.roll_back(id, &mut install, &mut rec)?,
        };
        lodestar_log::log_event!(RECOVERED, companion = self.label(id), step = op.step.code());
        Ok(outcome)
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

/// The real [`Run`]: CMP-4a's supervisor inside CMP-4b's sandbox, each
/// companion behind its route's gate.
pub struct Supervised {
    pub data: PathBuf,
    pub supervisor: tokio::sync::Mutex<Supervisor>,
    pub sandbox: Sandbox,
    pub http: Arc<dyn GuardedHttp>,
    /// The companion's route (Q18), from [`monitored`].
    pub route: Arc<dyn Fn(&CompanionId) -> Route + Send + Sync>,
    /// Where qBittorrent saves, if set.
    pub downloads: Option<PathBuf>,
    gates: Mutex<HashMap<CompanionId, Gated>>,
}

impl Supervised {
    pub fn new(
        data: PathBuf,
        supervisor: Supervisor,
        sandbox: Sandbox,
        http: Arc<dyn GuardedHttp>,
        route: Arc<dyn Fn(&CompanionId) -> Route + Send + Sync>,
        downloads: Option<PathBuf>,
    ) -> Supervised {
        Supervised {
            data,
            supervisor: tokio::sync::Mutex::new(supervisor),
            sandbox,
            http,
            route,
            downloads,
            gates: Mutex::new(HashMap::new()),
        }
    }
}

impl Run for Supervised {
    fn start(&self, l: Launch) -> BoxFuture<'_, Result<(), &'static str>> {
        Box::pin(async move {
            let health = crate::detect::health(&l.id).ok_or("no_health")?;
            let spec = SpawnSpec {
                id: l.id.clone(),
                executable: l.executable,
                args: l.args,
                env: Vec::new(),
                data_dir: self.data.clone(),
                allow_privileged: false,
            };
            let conf = Confinement {
                install: l.install,
                downloads: self.downloads.clone(),
            };
            let (spec, _report) = self.sandbox.prepare(spec, &conf).map_err(|e| e.code())?;
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
            let gated = egress::add(&mut sup, c, Follows::of(&l.id), (self.route)(&l.id))
                .map_err(|e| e.code())?;
            self.gates
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(l.id, gated);
            Ok(())
        })
    }

    fn healthy<'a>(&'a self, id: &'a CompanionId, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let Some(h) = crate::detect::health(id) else {
                return false;
            };
            let deadline = tokio::time::Instant::now() + HEALTH_DEADLINE;
            while tokio::time::Instant::now() < deadline {
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
            // while it's removed.
            drop(
                self.gates
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(id),
            );
            self.supervisor.lock().await.remove(id).await;
        })
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
