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
        let home = crate::supervise::process::home_dir(&self.p.data, id);
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
        *rec = self.save(id, rec, Step::Files)?;
        Ok(issued.secret)
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

impl Manager {
    /// Takes the data folder's companion lock; [`Refusal::Busy`] while
    /// another process (serve, or a `lodestar companion` command) holds it.
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
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(Refusal::Busy.into()),
            Err(std::fs::TryLockError::Error(_)) => return Err(failed("lock")("io")),
        }
        Ok(Manager(Arc::new(Inner {
            p: parts,
            _lock: lock,
            busy: Mutex::new(Vec::new()),
            snapshot: watch::channel(Snapshot::default()).0,
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
        let mut rec = self.off(move |m| m.begin(entry, pin)).await?;
        match self.steps(entry, artifact, pin, &mut rec).await {
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
                Ok(pin.version.clone())
            }
            Err(e) => {
                let (step, code) = match &e {
                    ManagerError::Step { step, code } => (*step, *code),
                    other => ("install", other.code()),
                };
                lodestar_log::log_event!(
                    INSTALL_FAILED,
                    companion = m.label(id),
                    version = pin.version.as_str(),
                    step = step,
                    reason = code
                );
                self.undo(entry, rec).await;
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
        m.p.run.stop(id).await;
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
        fault!(m, After(Step::Configured));
        let mut next = rec.clone();
        next.installed = Some(v.clone());
        *rec = self
            .off(move |m| m.save(id, &next, Step::Configured))
            .await?;

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
    async fn undo(&self, entry: &'static Entry, rec: Record) {
        let m = &self.0;
        let id = &entry.id;
        // Before `staged` the running version was never stopped.
        let touched = rec.op.as_ref().is_some_and(|o| o.step >= Step::Staged);
        if touched {
            m.p.run.stop(id).await;
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
                m.publish(id, Some((Some(prev), health)));
            }
            Ok(None) => m.publish(id, None),
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
        let secrets = self.off(move |m| m.secret(id)).await?;
        self.launch(entry, version, &exe, &target, secrets.into_iter().collect())
            .await?;
        let secret = self.off(move |m| m.secret(id)).await?;
        let opened = Opened {
            field: field_name(id),
            secret,
        };
        m.p.api
            .configure(id, &target, InstallReceipt::new(target.clone()), &opened)
            .await
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
        let (what, rec) = self.off(move |m| m.begin_remove(id)).await?;
        self.finish_remove(entry, rec).await?;
        Ok(what)
    }

    async fn finish_remove(&self, entry: &'static Entry, rec: Record) -> Result<(), ManagerError> {
        let m = &self.0;
        let id = &entry.id;
        fault!(m, Before(Step::Stopped));
        m.p.run.stop(id).await;
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
            }
            out.push((entry.id.clone(), r));
        }
        out
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
