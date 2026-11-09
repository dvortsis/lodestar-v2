//! `lodestar update-helper`: the MSI's install, run as SYSTEM by the
//! `Lodestar update helper` task (PK-win-upd-2; plan Q13, owner §2.47).
//! The pure parts and the folder layout are in [`super::msi`].
//!
//! The service account can write everything in the data folder, so a
//! helper that trusted any of it would turn that into SYSTEM. In order:
//!
//! 1. **Its own facts first.** This build's version must parse (else
//!    every version would look newer), and the helper must be
//!    `%ProgramFiles%\Lodestar\lodestar.exe` ([`Context::new`]).
//! 2. **Only a checked data folder:** the service's default,
//!    `%ProgramData%\Lodestar`, whose ACL and reparse points are checked
//!    as the service checks them at start. `--config` and
//!    `LODESTAR_DATA_DIR` are never read.
//! 3. **Pinned folders.** `<data>` and `<data>\updates` are opened as
//!    themselves (never through a link or junction) and held without
//!    `FILE_SHARE_DELETE` for the whole run, and `<data>\bin` the same way
//!    once the service has stopped ([`layout::pin_dir`], [`Bin`]), so
//!    nothing the helper reads or writes beneath them can be redirected.
//!    It deletes nothing in the data folder: the service cleans
//!    `updates\`.
//! 4. **The request** (`<data>\updates\request.json`) is parsed as
//!    untrusted input and acted on once: its id is recorded in staging
//!    whatever becomes of it, and the answer goes to
//!    `<data>\updates\result.json`. A version not newer than this build
//!    is refused (rule 4).
//! 5. **Copy, then verify** (rule 1): the envelope and the MSI are copied
//!    into the SYSTEM-only staging folder, and only the copies are read
//!    after that. The envelope is verified against the embedded keys, the
//!    request's channel and the clock (rule 5), over the helper's own
//!    `trust.json` in staging, so a validly signed older manifest is
//!    refused whatever the data folder holds. The MSI's copy must match
//!    the pinned size and SHA-256, and an Authenticode signature, if it
//!    carries one, must verify (§2.23).
//! 6. **Stop the service and take `<data>\bin\.lock`,** which the
//!    service holds for its whole run, then let go of it: the lock proves
//!    the service has stopped.
//! 7. **Install the copy** with fixed arguments (rule 6) and an
//!    environment without `LODESTAR_*`. The MSI starts the service again.
//!    A failed install marks the version `failed` in `skipped` (under the
//!    lock) and starts the old service; a busy Windows Installer skips
//!    nothing.
//! 8. **Probation** ([`super::rollback`]): a journal in staging is
//!    written before `msiexec` runs. The new service must reach Running
//!    and stay there for [`HEALTH_HOLD`], within [`HEALTH_WAIT`]. The
//!    installed MSI and its pins move into staging's `current\` (the one
//!    before into `previous\`). A failed probation rolls back: the store
//!    first, then the previous MSI, with the service disabled meanwhile. After exit 3010, and after a crash or a restart, the next
//!    run (at boot, the task's trigger) finishes the journal before it
//!    reads any request.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lodestar_update::doc::{ArtifactKind, MAX_ENVELOPE, Target};
use lodestar_update::semver::Version;
use lodestar_update::{Embedded, Sha256};
use sha2::Digest;

use super::check::{self as win, SKIP_FAILED};
use super::msi::{self, ENVELOPE, Installed, PINS, Pins, REQUEST, Refusal, Staging, UPDATES};
use super::rollback;
use crate::update::layout::{self, Bin, Lock};
use crate::update::trust_file;

/// How long the service may take to stop.
pub const STOP_WAIT: Duration = Duration::from_secs(120);
/// How long `.lock` may stay held once the service has stopped.
pub const LOCK_WAIT: Duration = Duration::from_secs(30);
/// How long the new service may take to reach Running.
pub const HEALTH_WAIT: Duration = Duration::from_secs(5 * 60);
/// How long it must stay Running to count as healthy.
pub const HEALTH_HOLD: Duration = Duration::from_secs(60);

/// What the helper knows about itself; none of it comes from the data
/// folder.
#[derive(Debug, Clone)]
pub struct Context {
    /// `%ProgramData%\Lodestar`, the service's default.
    pub data_dir: PathBuf,
    pub staging: Staging,
    /// This build's version; `None` when it doesn't parse.
    pub running: Option<Version>,
    pub target: Option<Target>,
    pub now: u64,
    pub embedded: Embedded,
    pub lock_wait: Duration,
}

impl Context {
    /// The context for `exe`, refused unless it's
    /// `<program files>\Lodestar\lodestar.exe`.
    pub fn new(
        exe: &Path,
        program_files: Option<&Path>,
        program_data: Option<String>,
        version: &str,
        target: Option<Target>,
        now: u64,
        embedded: Embedded,
    ) -> Result<Context, Refusal> {
        let signals = win::Signals {
            service_account: true,
            exe: Some(exe.to_path_buf()),
            program_files: program_files.map(Path::to_path_buf),
            ..win::Signals::default()
        };
        if win::detect(&signals) != win::InstallKind::Msi {
            return Err(Refusal::NotInstalled);
        }
        let install = exe.parent().ok_or(Refusal::NotInstalled)?;
        Ok(Context {
            data_dir: crate::windows::service::default_data_dir(program_data),
            staging: Staging::of(install),
            running: Version::parse(version).ok(),
            target,
            now,
            embedded,
            lock_wait: LOCK_WAIT,
        })
    }

    fn updates(&self) -> PathBuf {
        self.data_dir.join(UPDATES)
    }
}

/// What the system does for the helper: Windows, or a test's fake.
pub trait System {
    /// Why the data folder isn't private, as the service checks it.
    fn check_data(&self, dir: &Path) -> Result<(), String>;
    /// Creates the staging folder private to SYSTEM and Administrators,
    /// or checks that it is.
    fn secure_staging(&self, dir: &Path) -> Result<(), String>;
    /// Refuses an MSI whose Authenticode signature doesn't verify; one
    /// with none passes (§2.23).
    fn authenticode(&self, msi: &Path) -> Result<(), String>;
    /// Stops the service, returning once it has stopped.
    fn stop_service(&self) -> io::Result<()>;
    fn start_service(&self);
    /// Runs `msiexec` with `args`; its exit code.
    fn msiexec(&self, args: &[OsString]) -> io::Result<i32>;
    /// Whether the service reaches Running within [`HEALTH_WAIT`] and
    /// stays there for [`HEALTH_HOLD`].
    fn healthy(&self) -> bool;
    /// Sets the service's start type.
    fn set_start(&self, start: Start) -> io::Result<()>;
    /// The service's start type now.
    fn start_type(&self) -> io::Result<Start>;
    /// Writes an error to the Windows Event Log (Application).
    fn event_log(&self, message: &str);
    /// How many processes run as the service's account.
    fn service_processes(&self) -> io::Result<usize>;
    /// Called once each copy into staging is written (tests change the
    /// data folder here).
    fn copied(&self, _name: &str) {}
}

/// A service start type ([`System::set_start`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// During a rollback, and after a halt: the SCM's failure actions
    /// can't start it either.
    Disabled,
    /// Started by hand only.
    Manual,
    /// Delayed automatic, as the MSI installs it.
    Auto,
    /// Automatic, not delayed.
    Immediate,
}

/// What a run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No request.
    Nothing,
    /// Installed and healthy; `restart` when Windows must restart to
    /// finish replacing an in-use file.
    Installed { version: Version, restart: bool },
    /// `msiexec` failed and rolled back; the version is skipped.
    Failed { version: Version, code: i32 },
    /// Windows Installer was busy (another install); nothing changed and
    /// nothing is skipped.
    InstallerBusy { version: Version, code: i32 },
    /// Installed, but probation failed, and the rollback restored the
    /// store and reinstalled `to` ([`super::rollback`]); `healthy` is the
    /// old version's own health afterwards.
    RolledBack {
        failed: String,
        to: String,
        healthy: bool,
    },
    /// The rollback stopped for now (transient); the next run retries.
    RollbackPending { why: String },
    /// Probation failed and the rollback couldn't go on: `recovery` says
    /// what an administrator does.
    RollbackHalted { why: String, recovery: String },
    /// An install a crash interrupted never happened: the old version is
    /// still the one on disk.
    InstallInterrupted { version: String },
}

impl Outcome {
    pub fn code(&self) -> &'static str {
        match self {
            Outcome::Nothing => "nothing",
            Outcome::Installed { .. } => "installed",
            Outcome::Failed { .. } => "install_failed",
            Outcome::InstallerBusy { .. } => "installer_busy",
            Outcome::RolledBack { .. } => "rolled_back",
            Outcome::RollbackPending { .. } => "rollback_pending",
            Outcome::RollbackHalted { .. } => "rollback_halted",
            Outcome::InstallInterrupted { .. } => "install_interrupted",
        }
    }
}

fn io_refusal(what: &str) -> impl Fn(io::Error) -> Refusal + '_ {
    move |e| Refusal::Io(format!("{what}: {e}"))
}

/// One run: the steps in the module docs. A journal in staging (a
/// probation or rollback a restart or crash interrupted) is finished
/// first, before any request ([`rollback::resume`]).
pub fn run(ctx: &Context, sys: &dyn System) -> Result<Outcome, Refusal> {
    let running = ctx.running.clone().ok_or(Refusal::BuildVersion)?;
    let updates = ctx.updates();
    let journal_here = std::fs::symlink_metadata(ctx.staging.dir.join(rollback::JOURNAL)).is_ok();
    // Cheap first: no journal and no request, nothing to check (as at
    // most boots).
    if !journal_here && std::fs::symlink_metadata(updates.join(REQUEST)).is_err() {
        return Ok(Outcome::Nothing);
    }
    let data_pin =
        layout::pin_dir(&ctx.data_dir).map_err(|e| Refusal::UncheckedBin(e.to_string()))?;
    sys.check_data(&ctx.data_dir)
        .map_err(Refusal::UncheckedBin)?;
    let staging = &ctx.staging;
    sys.secure_staging(&staging.dir).map_err(Refusal::Staging)?;
    if let Some(journal) = rollback::read(ctx)? {
        match rollback::resume(ctx, sys, &running) {
            // Dropped (another version was installed by hand): go on.
            Ok(None) => {}
            result => {
                let result = result.map(|o| o.unwrap_or(Outcome::Nothing));
                if let Ok(updates_pin) = layout::pin_dir(&updates) {
                    answer(ctx, &updates, &journal.id, &journal.to, &result);
                    drop(updates_pin);
                }
                drop(data_pin);
                return result;
            }
        }
    }
    if std::fs::symlink_metadata(updates.join(REQUEST)).is_err() {
        return Ok(Outcome::Nothing);
    }
    let updates_pin =
        layout::pin_dir(&updates).map_err(|e| Refusal::UncheckedBin(e.to_string()))?;
    let Some(bytes) =
        read_capped(&updates.join(REQUEST), msi::MAX_REQUEST).map_err(io_refusal("request"))?
    else {
        return Ok(Outcome::Nothing);
    };
    let (request, version) = msi::parse_request(&bytes)?;
    let handled = layout::read_json::<Handled>(&staging.dir.join(msi::HANDLED))
        .ok()
        .flatten();
    if handled.is_some_and(|h| h.id == request.id) {
        return Ok(Outcome::Nothing);
    }
    // Acted on once, whatever happens next.
    let bytes = serde_json::to_vec(&Handled {
        id: request.id.clone(),
    })
    .map_err(|e| Refusal::Io(e.to_string()))?;
    layout::write_atomic(&staging.dir, msi::HANDLED, &bytes).map_err(io_refusal("handled"))?;
    let result = attempt(ctx, sys, &running, &request, &version);
    answer(ctx, &updates, &request.id, &version.to_string(), &result);
    drop((updates_pin, data_pin));
    result
}

/// Writes the answer to request `id` in `result.json`, beneath the pinned
/// `updates` folder. Display only: the service can write it too, so
/// nothing (no rollback) ever reads it back.
fn answer(
    ctx: &Context,
    updates: &Path,
    id: &str,
    version: &str,
    result: &Result<Outcome, Refusal>,
) {
    let answer = msi::Answer {
        format: 1,
        id: id.to_string(),
        version: version.to_string(),
        outcome: match result {
            Ok(o) => o.code().to_string(),
            Err(_) => "refused".into(),
        },
        code: match result {
            Ok(Outcome::Failed { code, .. } | Outcome::InstallerBusy { code, .. }) => {
                Some(code.to_string())
            }
            Ok(_) => None,
            Err(r) => Some(r.code().to_string()),
        },
        restart: matches!(result, Ok(Outcome::Installed { restart: true, .. })),
        detail: match result {
            Ok(Outcome::RollbackHalted { recovery, .. }) => Some(recovery.clone()),
            _ => None,
        },
        at: ctx.now,
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&answer) {
        let _ = layout::write_atomic(updates, msi::RESULT, &bytes);
    }
}

/// `handled.json` in staging.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Handled {
    id: String,
}

/// The steps after the request is parsed and recorded.
fn attempt(
    ctx: &Context,
    sys: &dyn System,
    running: &Version,
    request: &msi::Request,
    version: &Version,
) -> Result<Outcome, Refusal> {
    let version = version.clone();
    if !msi::newer(running, &version) {
        return Err(Refusal::NotNewer);
    }
    let target = ctx.target.ok_or(Refusal::NoArtifact)?;
    let updates = ctx.updates();
    let staging = &ctx.staging;
    let incoming = staging.incoming();
    remove_dir(&incoming).map_err(io_refusal("incoming"))?;
    std::fs::create_dir(&incoming).map_err(io_refusal("incoming"))?;

    // The envelope: copied, then verified from the copy.
    let envelope = copy_capped(
        &updates.join(ENVELOPE),
        &incoming.join(ENVELOPE),
        MAX_ENVELOPE as u64,
    )
    .map_err(io_refusal("manifest"))?;
    sys.copied(ENVELOPE);
    let envelope = read_capped(&envelope, MAX_ENVELOPE as u64)
        .map_err(io_refusal("manifest"))?
        .ok_or_else(|| Refusal::Io("manifest: gone from staging".into()))?;
    let loaded = trust_file::load(&staging.dir, &ctx.embedded, &[]);
    if loaded.damaged() {
        return Err(Refusal::Manifest("trust_damaged"));
    }
    let floor = trust_floor(ctx, loaded.state());
    let checked =
        lodestar_update::check(&envelope, request.channel, ctx.now, &ctx.embedded, &floor)
            .map_err(|e| Refusal::Manifest(e.code()))?;
    // Forward only: an older manifest is refused from now on.
    trust_file::save(&staging.dir, &loaded, checked.next.clone())
        .map_err(io_refusal("trust.json"))?;
    let pin = lodestar_update::install(&checked, &version, target, ArtifactKind::Msi).map_err(
        |e| match e {
            lodestar_update::Error::NoArtifact => Refusal::NoArtifact,
            e => Refusal::Manifest(e.code()),
        },
    )?;

    // The MSI: copied, then the copy checked.
    let name = msi::msi_name(&version);
    let staged = match copy_capped(&updates.join(&name), &incoming.join(&name), pin.size) {
        Err(e) if e.kind() == io::ErrorKind::FileTooLarge => return Err(Refusal::Mismatch),
        other => other.map_err(io_refusal("msi"))?,
    };
    sys.copied(&name);
    let (sha256, size) = hash(&staged).map_err(io_refusal("msi"))?;
    if size != pin.size || sha256 != pin.sha256 {
        return Err(Refusal::Mismatch);
    }
    sys.authenticode(&staged).map_err(Refusal::Authenticode)?;
    let pins = Pins {
        version: version.to_string(),
        channel: request.channel,
        sha256: pin.sha256.to_hex(),
        size: pin.size,
    };

    // The service stops; `.lock` proves it.
    if sys.stop_service().is_err() {
        // It may still stop late: start it either way.
        sys.start_service();
        return Err(Refusal::Busy("service_running"));
    }
    let locked = Bin::existing(&ctx.data_dir)
        .map_err(io_refusal("bin"))
        .and_then(|b| b.ok_or(Refusal::Busy("bin_missing")))
        .and_then(|bin| take_lock(&bin, ctx.lock_wait).map(drop));
    if let Err(e) = locked {
        sys.start_service();
        return Err(e);
    }
    // Nothing may run as the service's account while SYSTEM installs: the
    // MSI's custom actions walk the data folder (#640's security review).
    if rollback::no_service_processes(sys).is_err() {
        sys.start_service();
        return Err(Refusal::Busy("service_processes"));
    }
    // Before msiexec: a crash from here on is finished by the next run.
    let journal = match rollback::installing(ctx, &request.id, running, &version)
        .and_then(|j| rollback::write(ctx, &j).map(|()| j))
    {
        Ok(j) => j,
        Err(e) => {
            sys.start_service();
            return Err(e);
        }
    };

    let args = msi::msiexec_args(&staged, &staging.log());
    let code = match sys.msiexec(&args) {
        Ok(c) => c,
        Err(_) => {
            // Never started: nothing changed.
            rollback::clear(ctx);
            sys.start_service();
            return Err(Refusal::Busy("msiexec_unavailable"));
        }
    };
    match msi::installed(code) {
        Installed::Failed(code) => {
            // Windows Installer rolled it back: nothing to roll back here.
            rollback::clear(ctx);
            mark_failed(ctx, &version);
            sys.start_service();
            Ok(Outcome::Failed { version, code })
        }
        Installed::Busy(code) => {
            rollback::clear(ctx);
            sys.start_service();
            Ok(Outcome::InstallerBusy { version, code })
        }
        done => {
            // Installed: a staging problem doesn't undo that, so it's
            // reported and probation still runs.
            if let Err(e) = keep(staging, &staged, &pins) {
                eprintln!("lodestar update-helper: keeping the MSI for rollback: {e}");
            }
            Ok(rollback::probation(
                ctx,
                sys,
                journal,
                done == Installed::RestartNeeded,
            ))
        }
    }
}

/// The trust state a check runs over: the helper's own, raised (never
/// lowered) by the data folder's `trust.json` when it verifies. On a first
/// install the helper has seen no manifest yet; the service's marks keep
/// a replayed older, still unexpired manifest out (#640's security
/// review). The service can only raise them, which refuses more.
///
/// Its signed statements and channel marks are merged forward-only; never
/// its revocation list, which the service could pad (an entry need only
/// parse as a key). Marks more than a day ahead fail the check's own
/// plausibility rule: the service can delay installs, never allow one.
fn trust_floor(ctx: &Context, own: &lodestar_update::TrustState) -> lodestar_update::TrustState {
    let data = trust_file::load(&ctx.data_dir.join("bin"), &ctx.embedded, &[]);
    if data.damaged() {
        return own.clone();
    }
    // Its statements come too (each verified under the embedded recovery
    // key when it loaded), so a rotation it saw doesn't reset the marks it
    // raised; its revocation list never does.
    let mut theirs = data.state().clone();
    theirs.revoked.clear();
    lodestar_update::TrustState::merge(own, theirs)
}

/// Takes `.lock`, waiting up to `wait` for the stopped service's handle
/// to go.
pub(super) fn take_lock(bin: &Bin, wait: Duration) -> Result<Lock, Refusal> {
    let deadline = Instant::now() + wait;
    loop {
        if let Some(l) = bin.lock().map_err(io_refusal(".lock"))? {
            return Ok(l);
        }
        if Instant::now() >= deadline {
            return Err(Refusal::Busy("bin_busy"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A failed install's version is never offered again: `skipped`, reason
/// `failed`, written under `.lock` in the pinned `<data>\bin`. Best
/// effort: when the lock or the file isn't available the next check
/// offers it again, and it fails the same way.
fn mark_failed(ctx: &Context, version: &Version) {
    let Ok(Some(bin)) = Bin::existing(&ctx.data_dir) else {
        return;
    };
    let Ok(Some(_lock)) = bin.lock() else {
        return;
    };
    let _ = win::add_skip(&bin, &version.to_string(), SKIP_FAILED);
}

/// Moves the installed MSI and its pins to `current\`, and the one
/// before to `previous\`.
fn keep(staging: &Staging, staged: &Path, pins: &Pins) -> io::Result<()> {
    let (current, previous) = (staging.current(), staging.previous());
    if current.exists() {
        remove_dir(&previous)?;
        layout::rename_retrying(&current, &previous)?;
    }
    std::fs::create_dir(&current)?;
    let name = staged
        .file_name()
        .ok_or_else(|| io::Error::other("no name"))?;
    layout::rename_retrying(staged, &current.join(name))?;
    let bytes = serde_json::to_vec_pretty(pins).map_err(io::Error::other)?;
    layout::write_atomic(&current, PINS, &bytes)
}

/// Reads at most `cap` bytes of a file opened as itself; `None` when it's
/// missing, an error past `cap`.
fn read_capped(path: &Path, cap: u64) -> io::Result<Option<Vec<u8>>> {
    let file = match layout::open_nofollow(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too large"));
    }
    Ok(Some(bytes))
}

/// Copies `from` (opened as itself) to a new file `to`, at most `cap`
/// bytes (one more is an error); flushed. Returns `to`.
fn copy_capped(from: &Path, to: &Path, cap: u64) -> io::Result<PathBuf> {
    let src = layout::open_nofollow(from)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let n = io::copy(&mut src.take(cap + 1), &mut dst)?;
    if n > cap {
        drop(dst);
        let _ = std::fs::remove_file(to);
        return Err(io::Error::new(io::ErrorKind::FileTooLarge, "too large"));
    }
    dst.flush()?;
    dst.sync_all()?;
    Ok(to.to_path_buf())
}

/// The SHA-256 and size of a staged file.
pub(super) fn hash(path: &Path) -> io::Result<(Sha256, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        size += n as u64;
    }
    Ok((Sha256(h.finalize().into()), size))
}

pub(super) fn remove_dir(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// [`System`] on Windows. Every program is started by absolute path from
/// `System32`, without a window and with no `LODESTAR_*` variable in its
/// environment (the relaunch token among them).
#[cfg(windows)]
pub struct Windows;

#[cfg(windows)]
impl Windows {
    fn command(relative: &str) -> io::Result<std::process::Command> {
        use std::os::windows::process::CommandExt;
        /// `CREATE_NO_WINDOW`.
        const NO_WINDOW: u32 = 0x0800_0000;
        let root = std::env::var_os("SystemRoot")
            .ok_or_else(|| io::Error::other("SystemRoot isn't set"))?;
        let mut cmd = std::process::Command::new(Path::new(&root).join("System32").join(relative));
        cmd.creation_flags(NO_WINDOW);
        clean_env(&mut cmd);
        Ok(cmd)
    }

    fn sc(args: &[&str]) -> io::Result<String> {
        let out = Self::command("sc.exe")?.args(args).output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// How many processes run as `account` (`tasklist` filtered by it).
    pub fn processes_of(account: &str) -> io::Result<usize> {
        let filter = format!("USERNAME eq {account}");
        let out = Self::command("tasklist.exe")?
            .args(["/fi", &filter, "/fo", "csv", "/nh"])
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other("tasklist failed"));
        }
        Ok(count_rows(&String::from_utf8_lossy(&out.stdout)))
    }

    /// Polls `sc query` until `state` (`sc`'s state words aren't
    /// translated), or `wait` passes.
    fn wait_for(state: &str, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        loop {
            if Self::sc(&["query", crate::windows::service::NAME])
                .is_ok_and(|out| service_state(&out).as_deref() == Some(state))
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

/// Removes every `LODESTAR_*` variable (the relaunch token among them)
/// from what `cmd` starts with.
pub fn clean_env(cmd: &mut std::process::Command) {
    clean_env_of(cmd, std::env::vars_os().map(|(k, _)| k));
}

/// [`clean_env`] over the variable names `names`.
fn clean_env_of(cmd: &mut std::process::Command, names: impl Iterator<Item = OsString>) {
    for k in names {
        if k.to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("LODESTAR_")
        {
            cmd.env_remove(k);
        }
    }
}

/// The start type in `sc qc` output (`START_TYPE : 2   AUTO_START
/// (DELAYED)`; the words aren't translated).
pub fn start_type_of(text: &str) -> Option<Start> {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("START_TYPE"))?;
    let (_, rest) = line.split_once(':')?;
    Some(match rest.split_whitespace().next()? {
        "2" if rest.contains("DELAYED") => Start::Auto,
        "2" => Start::Immediate,
        "3" => Start::Manual,
        "4" => Start::Disabled,
        _ => return None,
    })
}

/// The CSV rows in `tasklist /fo csv /nh` output.
pub fn count_rows(text: &str) -> usize {
    text.lines()
        .filter(|l| l.trim_start().starts_with('"'))
        .count()
}

/// The state word in `sc query` output (`STATE : 4  RUNNING`).
pub fn service_state(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (key, rest) = l.split_once(':')?;
        if key.trim() != "STATE" {
            return None;
        }
        rest.split_whitespace().nth(1).map(str::to_string)
    })
}

#[cfg(windows)]
impl System for Windows {
    fn check_data(&self, dir: &Path) -> Result<(), String> {
        use crate::windows::acl::{self, Ops};
        let own = acl::service_sid(crate::windows::service::NAME)
            .map_err(|e| format!("the service's SID: {e}"))?;
        let ops = acl::Windows {
            own_sid: Some(&own),
        };
        let seen = ops
            .inspect(dir)
            .map_err(|e| format!("reading its permissions: {e}"))?
            .ok_or("it doesn't exist")?;
        acl::verdict(&seen, Some(&own))
    }

    fn secure_staging(&self, dir: &Path) -> Result<(), String> {
        use crate::windows::acl;
        acl::secure_dir(dir, None, &acl::Windows { own_sid: None })
    }

    fn authenticode(&self, msi: &Path) -> Result<(), String> {
        let out = Self::command(r"WindowsPowerShell\v1.0\powershell.exe")
            .map_err(|e| e.to_string())?
            .env_remove("PSModulePath")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                AUTHENTICODE_SCRIPT,
            ])
            .env("LODESTAR_HELPER_MSI", msi)
            .output()
            .map_err(|e| e.to_string())?;
        authenticode_verdict(&String::from_utf8_lossy(&out.stdout))
    }

    fn stop_service(&self) -> io::Result<()> {
        let _ = Self::sc(&["stop", crate::windows::service::NAME])?;
        if Self::wait_for("STOPPED", STOP_WAIT) {
            Ok(())
        } else {
            Err(io::Error::other("the service didn't stop"))
        }
    }

    fn start_service(&self) {
        let _ = Self::sc(&["start", crate::windows::service::NAME]);
    }

    fn set_start(&self, start: Start) -> io::Result<()> {
        let mode = match start {
            Start::Disabled => "disabled",
            Start::Manual => "demand",
            Start::Auto => "delayed-auto",
            Start::Immediate => "auto",
        };
        let out = Self::command("sc.exe")?
            .args(["config", crate::windows::service::NAME, "start=", mode])
            .output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other("sc config failed"))
        }
    }

    fn start_type(&self) -> io::Result<Start> {
        let out = Self::sc(&["qc", crate::windows::service::NAME])?;
        start_type_of(&out).ok_or_else(|| io::Error::other("sc qc gave no start type"))
    }

    /// Through `eventcreate` (source "Lodestar update helper"), best effort.
    fn event_log(&self, message: &str) {
        let _ = Self::command("eventcreate.exe").and_then(|mut c| {
            c.args([
                "/T",
                "ERROR",
                "/ID",
                "900",
                "/L",
                "APPLICATION",
                "/SO",
                "Lodestar update helper",
                "/D",
                message,
            ])
            .output()
        });
    }

    /// `tasklist`'s CSV rows for the service's account (the filter's
    /// words and the account name aren't translated; a row starts with a
    /// quote, the "no tasks" message doesn't).
    fn service_processes(&self) -> io::Result<usize> {
        Self::processes_of(&format!("NT SERVICE\\{}", crate::windows::service::NAME))
    }

    fn msiexec(&self, args: &[OsString]) -> io::Result<i32> {
        let status = Self::command("msiexec.exe")?.args(args).status()?;
        status
            .code()
            .ok_or_else(|| io::Error::other("msiexec ended without a code"))
    }

    /// Running within [`HEALTH_WAIT`], then still Running at every look
    /// for [`HEALTH_HOLD`]: a service that starts and then crashes isn't
    /// healthy.
    fn healthy(&self) -> bool {
        if !Self::wait_for("RUNNING", HEALTH_WAIT) {
            return false;
        }
        let until = Instant::now() + HEALTH_HOLD;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_secs(2));
            let running = Self::sc(&["query", crate::windows::service::NAME])
                .is_ok_and(|out| service_state(&out).as_deref() == Some("RUNNING"));
            if !running {
                return false;
            }
        }
        true
    }
}

/// `Get-AuthenticodeSignature`'s status for the MSI (the path arrives in
/// an environment variable, so it's never parsed as script).
pub const AUTHENTICODE_SCRIPT: &str = "$ErrorActionPreference = 'Stop'
(Get-AuthenticodeSignature -LiteralPath $env:LODESTAR_HELPER_MSI).Status.ToString()";

/// §2.23: unsigned passes, a valid signature passes, anything else (a
/// signature that doesn't verify, or no answer) is refused.
pub fn authenticode_verdict(status: &str) -> Result<(), String> {
    match status.trim() {
        "NotSigned" | "Valid" => Ok(()),
        "" => Err("its signature couldn't be read".into()),
        other => Err(format!("its signature is {other}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use lodestar_update::Channel;
    use lodestar_update::testing::{TestTrust, bytes, envelope, manifest};
    use serde_json::{Value, json};

    use super::*;

    const NOW: u64 = 1_800_000_000;
    const MSI: &[u8] = b"the 0.7.0 msi";

    /// An action a test runs at one of the helper's hooks.
    type Hook = Box<dyn Fn() + Send + Sync>;

    #[derive(Default)]
    struct Fake {
        data_private: Option<String>,
        staging_private: Option<String>,
        authenticode: Option<String>,
        stop_fails: bool,
        code: i32,
        unhealthy: bool,
        /// Processes still running as the service's account.
        processes: usize,
        /// The data folder, for the first msiexec to "migrate" the store:
        /// a snapshot of it, then a changed store.
        migrate: Option<PathBuf>,
        /// Panics at this call (0-based), as a crash would.
        crash_at: Option<usize>,
        /// `lodestar.db` at each msiexec.
        store_seen: Mutex<Vec<Option<Vec<u8>>>>,
        health_looks: Mutex<usize>,
        /// The start type before the update (`Auto` when unset).
        start_before: Option<Start>,
        /// What went to the Event Log.
        events: Mutex<Vec<String>>,
        /// Called at a test hook (`copied`): (hook name, action).
        at_hook: Option<(&'static str, Hook)>,
        /// A source to overwrite once it's copied: (name, path, bytes).
        swap: Option<(String, PathBuf, Vec<u8>)>,
        calls: Mutex<Vec<String>>,
        /// What msiexec was given, and the bytes of the MSI it was given.
        ran: Mutex<Option<(Vec<OsString>, Vec<u8>)>>,
    }

    impl Fake {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn note(&self, s: &str) {
            let n = {
                let mut c = self.calls.lock().unwrap();
                c.push(s.into());
                c.len() - 1
            };
            if self.crash_at == Some(n) {
                panic!("crash at call {n} ({s})");
            }
        }
    }

    impl System for Fake {
        fn check_data(&self, _: &Path) -> Result<(), String> {
            self.data_private.clone().map_or(Ok(()), Err)
        }
        fn secure_staging(&self, dir: &Path) -> Result<(), String> {
            if let Some(why) = &self.staging_private {
                return Err(why.clone());
            }
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())
        }
        fn authenticode(&self, _: &Path) -> Result<(), String> {
            self.authenticode.clone().map_or(Ok(()), Err)
        }
        fn stop_service(&self) -> io::Result<()> {
            self.note("stop");
            if self.stop_fails {
                Err(io::Error::other("no"))
            } else {
                Ok(())
            }
        }
        fn start_service(&self) {
            self.note("start");
        }
        fn msiexec(&self, args: &[OsString]) -> io::Result<i32> {
            self.note("msiexec");
            let msi = std::fs::read(&args[1]).unwrap();
            if let Some(data) = &self.migrate {
                let store = data.join("lodestar.db");
                self.store_seen
                    .lock()
                    .unwrap()
                    .push(std::fs::read(&store).ok());
                let snap = data.join("lodestar.pre-v5.db");
                if !snap.exists() && self.ran.lock().unwrap().is_none() {
                    std::fs::copy(&store, &snap).unwrap();
                    std::fs::write(&store, b"migrated store").unwrap();
                }
            }
            *self.ran.lock().unwrap() = Some((args.to_vec(), msi));
            Ok(self.code)
        }
        /// Unhealthy at the first look only (the failed version), healthy
        /// after (the old one, once rolled back).
        fn healthy(&self) -> bool {
            let mut n = self.health_looks.lock().unwrap();
            *n += 1;
            !(self.unhealthy && *n == 1)
        }
        fn set_start(&self, start: Start) -> io::Result<()> {
            self.note(match start {
                Start::Disabled => "disabled",
                Start::Manual => "manual",
                Start::Auto => "auto",
                Start::Immediate => "immediate",
            });
            Ok(())
        }
        fn start_type(&self) -> io::Result<Start> {
            Ok(self.start_before.unwrap_or(Start::Auto))
        }
        fn event_log(&self, message: &str) {
            self.events.lock().unwrap().push(message.into());
            self.note("event");
        }
        fn service_processes(&self) -> io::Result<usize> {
            Ok(self.processes)
        }
        fn copied(&self, name: &str) {
            if let Some((hook, act)) = &self.at_hook
                && *hook == name
            {
                act();
            }
            self.note(&format!("copied {name}"));
            if let Some((n, path, bytes)) = &self.swap
                && n == name
            {
                std::fs::write(path, bytes).unwrap();
            }
        }
    }

    struct Env {
        _tmp: tempfile::TempDir,
        ctx: Context,
        trust: TestTrust,
    }

    impl Env {
        fn new(running: &str) -> Env {
            let tmp = tempfile::tempdir().unwrap();
            let trust = TestTrust::default();
            let ctx = Context {
                data_dir: tmp.path().join("data"),
                staging: Staging::of(&tmp.path().join("Program Files").join("Lodestar")),
                running: Version::parse(running).ok(),
                target: Some(Target::X86_64WindowsMsvc),
                now: NOW,
                embedded: trust.embedded(vec![]),
                lock_wait: Duration::from_millis(300),
            };
            std::fs::create_dir_all(ctx.updates()).unwrap();
            // The service has run: `<data>\bin` exists.
            drop(Bin::open(&ctx.data_dir).unwrap());
            Env {
                _tmp: tmp,
                ctx,
                trust,
            }
        }

        fn updates(&self, name: &str) -> PathBuf {
            self.ctx.updates().join(name)
        }

        /// An envelope offering `version`'s MSI pinned to `msi`, dated
        /// `ts`, on `channel`.
        fn envelope(&self, channel: Channel, ts: u64, version: &str, msi: &[u8]) -> Vec<u8> {
            let mut m = manifest(channel, ts, version, b"x");
            m["releases"][0]["artifacts"] = json!([{
                "target": "x86_64-pc-windows-msvc",
                "kind": "msi",
                "url": format!("https://github.com/dvortsis/lodestar-v2/releases/download/v{version}/l.msi"),
                "sha256": Sha256::of(msi).to_hex(),
                "size": msi.len(),
            }]);
            let key = match channel {
                Channel::Nightly => &self.trust.nightly,
                _ => &self.trust.release,
            };
            envelope(&key.signed(&bytes(&m)), None)
        }

        /// What the service leaves: the request (a new id each time), the
        /// envelope, the MSI.
        fn leave(&self, version: &str, channel: Channel, envelope: &[u8], msi: &[u8]) {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let id = format!("{:016x}", NEXT.fetch_add(1, Ordering::Relaxed));
            let req = json!({"format": 1, "id": id, "version": version, "channel": channel});
            std::fs::write(self.updates(REQUEST), bytes(&req)).unwrap();
            std::fs::write(self.updates(ENVELOPE), envelope).unwrap();
            let v = Version::parse(version).unwrap();
            std::fs::write(self.updates(&msi::msi_name(&v)), msi).unwrap();
        }

        fn offer(&self, version: &str) {
            let env = self.envelope(Channel::Stable, NOW - 60, version, MSI);
            self.leave(version, Channel::Stable, &env, MSI);
        }

        fn run(&self, fake: &Fake) -> Result<Outcome, Refusal> {
            run(&self.ctx, fake)
        }

        /// The helper's answer in `result.json`.
        fn answer(&self) -> msi::Answer {
            serde_json::from_slice(&std::fs::read(self.updates(msi::RESULT)).unwrap()).unwrap()
        }
    }

    fn refused(r: Result<Outcome, Refusal>) -> &'static str {
        r.unwrap_err().code()
    }

    #[test]
    fn installs_the_verified_copy_and_keeps_it() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake::default();
        let out = env.run(&fake).unwrap();
        assert_eq!(
            out,
            Outcome::Installed {
                version: Version::new(0, 7, 0),
                restart: false
            }
        );
        // Copy, then verify, then stop, then install.
        assert_eq!(
            fake.calls(),
            [
                "copied manifest.json",
                "copied lodestar-0.7.0.msi",
                "stop",
                "msiexec"
            ]
        );
        let (args, bytes) = fake.ran.lock().unwrap().clone().unwrap();
        let staged = env.ctx.staging.incoming().join("lodestar-0.7.0.msi");
        assert_eq!(
            args,
            msi::msiexec_args(&staged, &env.ctx.staging.log()),
            "fixed arguments, from staging"
        );
        assert_eq!(bytes, MSI);
        // Nothing in the data folder is deleted; the answer is there.
        assert!(env.updates(REQUEST).exists());
        assert!(env.updates("lodestar-0.7.0.msi").exists());
        let a = env.answer();
        assert_eq!(
            (a.outcome.as_str(), a.version.as_str(), a.code, a.restart),
            ("installed", "0.7.0", None, false)
        );
        // The MSI and its pins are kept for rollback.
        let current = env.ctx.staging.current();
        assert_eq!(
            std::fs::read(current.join("lodestar-0.7.0.msi")).unwrap(),
            MSI
        );
        let pins: Pins =
            serde_json::from_slice(&std::fs::read(current.join(PINS)).unwrap()).unwrap();
        assert_eq!(pins.sha256, Sha256::of(MSI).to_hex());
        assert_eq!(pins.size, MSI.len() as u64);
        // Nothing left to do on the next run (at boot, say).
        assert_eq!(env.run(&Fake::default()).unwrap(), Outcome::Nothing);
    }

    /// Rule 1: the MSI swapped in the data folder after the helper copied
    /// it is never installed: the copy is what's verified and run.
    #[test]
    fn a_file_swapped_after_the_copy_is_never_installed() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            swap: Some((
                "lodestar-0.7.0.msi".into(),
                env.updates("lodestar-0.7.0.msi"),
                b"evil".to_vec(),
            )),
            ..Fake::default()
        };
        env.run(&fake).unwrap();
        assert_eq!(fake.ran.lock().unwrap().clone().unwrap().1, MSI);
    }

    /// Swapped before the copy: the copy doesn't match the pin.
    #[test]
    fn a_wrong_size_or_hash_is_refused() {
        for evil in [&b"the 0.7.0 msj"[..], b"short", b"the 0.7.0 msi, and more"] {
            let env = Env::new("0.6.0");
            env.offer("0.7.0");
            std::fs::write(env.updates("lodestar-0.7.0.msi"), evil).unwrap();
            let fake = Fake::default();
            assert_eq!(refused(env.run(&fake)), "artifact_mismatch");
            assert!(fake.ran.lock().unwrap().is_none());
            assert!(!fake.calls().contains(&"stop".to_string()));
        }
    }

    /// A swapped envelope after the copy changes nothing either.
    #[test]
    fn the_envelope_is_verified_from_its_copy() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            swap: Some((ENVELOPE.into(), env.updates(ENVELOPE), b"{}".to_vec())),
            ..Fake::default()
        };
        assert_eq!(env.run(&fake).unwrap().code(), "installed");
    }

    /// A validly signed older manifest is refused once a newer one was
    /// seen, whatever the data folder's `trust.json` says: the helper
    /// keeps its own.
    #[test]
    fn a_replayed_older_manifest_is_refused() {
        let env = Env::new("0.6.0");
        // An old manifest offering 0.6.5, recorded before 0.7.0's.
        let old = env.envelope(Channel::Stable, NOW - 3600, "0.6.5", MSI);
        env.offer("0.7.0");
        // The run fails at msiexec, so 0.6.0 is still installed.
        let failing = Fake {
            code: 1603,
            ..Fake::default()
        };
        assert_eq!(env.run(&failing).unwrap().code(), "install_failed");
        // The replay: an older, validly signed, still-current manifest.
        env.leave("0.6.5", Channel::Stable, &old, MSI);
        std::fs::remove_file(env.ctx.data_dir.join("bin").join("trust.json")).ok();
        let fake = Fake::default();
        assert_eq!(refused(env.run(&fake)), "older_than_mark");
        assert!(fake.ran.lock().unwrap().is_none());
    }

    #[test]
    fn a_version_that_isnt_newer_is_refused() {
        for (running, offered) in [
            ("0.7.0", "0.7.0"),
            ("0.7.0", "0.6.9"),
            ("0.7.0", "0.7.0-rc.1"),
        ] {
            let env = Env::new(running);
            env.offer(offered);
            let fake = Fake::default();
            assert_eq!(refused(env.run(&fake)), "not_newer", "{offered}");
            assert!(fake.calls().is_empty());
        }
    }

    #[test]
    fn an_unparsed_build_version_installs_nothing() {
        let env = Env::new("not a version");
        env.offer("0.7.0");
        assert_eq!(refused(env.run(&Fake::default())), "build_version_unknown");
    }

    #[test]
    fn an_unchecked_data_folder_installs_nothing() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            data_private: Some("it inherits".into()),
            ..Fake::default()
        };
        assert_eq!(refused(env.run(&fake)), "bin_unchecked");
        assert!(env.updates(REQUEST).exists());
    }

    #[test]
    fn a_staging_folder_that_isnt_private_installs_nothing() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            staging_private: Some("it belongs to BU".into()),
            ..Fake::default()
        };
        assert_eq!(refused(env.run(&fake)), "staging_not_private");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_manifest_for_another_channel_is_refused() {
        let env = Env::new("0.6.0");
        // Validly signed by the same key, for Beta.
        let beta = env.envelope(Channel::Beta, NOW - 60, "0.7.0", MSI);
        env.leave("0.7.0", Channel::Stable, &beta, MSI);
        assert_eq!(refused(env.run(&Fake::default())), "wrong_channel");
    }

    /// Rule 5: an expired manifest installs nothing.
    #[test]
    fn a_stale_manifest_is_refused() {
        let env = Env::new("0.6.0");
        let stale = env.envelope(Channel::Stable, NOW - 8 * 86_400, "0.7.0", MSI);
        env.leave("0.7.0", Channel::Stable, &stale, MSI);
        assert_eq!(refused(env.run(&Fake::default())), "not_current");
    }

    #[test]
    fn a_release_without_this_targets_msi_is_refused() {
        let env = Env::new("0.6.0");
        let mut m = manifest(Channel::Stable, NOW - 60, "0.7.0", b"x");
        m["releases"][0]["artifacts"][0]["kind"] = Value::from("binary");
        let e = envelope(&env.trust.release.signed(&bytes(&m)), None);
        env.leave("0.7.0", Channel::Stable, &e, MSI);
        assert_eq!(refused(env.run(&Fake::default())), "no_artifact");
        let mut env = Env::new("0.6.0");
        env.ctx.target = None;
        env.offer("0.7.0");
        assert_eq!(refused(env.run(&Fake::default())), "no_artifact");
    }

    #[test]
    fn a_damaged_request_or_signature_is_refused() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        std::fs::write(env.updates(REQUEST), b"{\"format\":1}").unwrap();
        assert_eq!(refused(env.run(&Fake::default())), "request_unreadable");
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let mut e: Value =
            serde_json::from_slice(&std::fs::read(env.updates(ENVELOPE)).unwrap()).unwrap();
        e["manifest"]["minisig"] = Value::from(TestTrust::default().nightly.sign(b"other"));
        std::fs::write(env.updates(ENVELOPE), bytes(&e)).unwrap();
        assert!(env.run(&Fake::default()).is_err());
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            authenticode: Some("its signature is HashMismatch".into()),
            ..Fake::default()
        };
        assert_eq!(refused(env.run(&fake)), "authenticode");
        assert!(fake.ran.lock().unwrap().is_none());
    }

    #[test]
    fn no_request_is_nothing_to_do() {
        let env = Env::new("0.6.0");
        assert_eq!(env.run(&Fake::default()).unwrap(), Outcome::Nothing);
        std::fs::remove_dir(env.ctx.updates()).unwrap();
        assert_eq!(env.run(&Fake::default()).unwrap(), Outcome::Nothing);
    }

    /// The service holds `.lock` for its whole run: while anything holds
    /// it, the helper writes nothing and starts the service again.
    #[test]
    fn the_lock_held_by_the_service_stops_the_install() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let bin = Bin::open(&env.ctx.data_dir).unwrap();
        let held = bin.lock().unwrap().unwrap();
        let fake = Fake::default();
        assert_eq!(refused(env.run(&fake)), "bin_busy");
        assert_eq!(fake.calls().last().unwrap(), "start");
        assert!(fake.ran.lock().unwrap().is_none());
        assert_eq!(
            (env.answer().outcome.as_str(), env.answer().code.as_deref()),
            ("refused", Some("bin_busy"))
        );
        drop(held);
        // That request was answered; a boot run doesn't repeat it.
        assert_eq!(env.run(&Fake::default()).unwrap(), Outcome::Nothing);
        // A new Install (a new id) does.
        env.offer("0.7.0");
        assert_eq!(env.run(&Fake::default()).unwrap().code(), "installed");
    }

    #[test]
    fn a_service_that_wont_stop_installs_nothing() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            stop_fails: true,
            ..Fake::default()
        };
        assert_eq!(refused(env.run(&fake)), "service_running");
        // Started anyway: it may stop late.
        assert_eq!(
            fake.calls(),
            [
                "copied manifest.json",
                "copied lodestar-0.7.0.msi",
                "stop",
                "start"
            ]
        );
    }

    /// A busy Windows Installer (another install running) skips nothing.
    #[test]
    fn a_busy_installer_skips_nothing() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            code: 1618,
            ..Fake::default()
        };
        assert_eq!(env.run(&fake).unwrap().code(), "installer_busy");
        assert_eq!(fake.calls().last().unwrap(), "start");
        assert!(!env.ctx.data_dir.join("bin").join("skipped").exists());
        assert_eq!(env.answer().code.as_deref(), Some("1618"));
    }

    /// Links in place of the folder or its files are never read through,
    /// and an oversize envelope is refused.
    #[cfg(unix)]
    #[test]
    fn links_and_oversize_files_are_refused() {
        // `updates` itself a link.
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let real = env.ctx.data_dir.join("real-updates");
        std::fs::rename(env.ctx.updates(), &real).unwrap();
        std::os::unix::fs::symlink(&real, env.ctx.updates()).unwrap();
        assert_eq!(refused(env.run(&Fake::default())), "bin_unchecked");
        // A link in place of each file.
        for name in [REQUEST, ENVELOPE, "lodestar-0.7.0.msi"] {
            let env = Env::new("0.6.0");
            env.offer("0.7.0");
            let elsewhere = env.ctx.data_dir.join("elsewhere");
            std::fs::rename(env.updates(name), &elsewhere).unwrap();
            std::os::unix::fs::symlink(&elsewhere, env.updates(name)).unwrap();
            let fake = Fake::default();
            assert_eq!(refused(env.run(&fake)), "io", "{name}");
            assert!(fake.ran.lock().unwrap().is_none(), "{name}");
        }
        // An envelope past the cap.
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        std::fs::write(env.updates(ENVELOPE), vec![b' '; MAX_ENVELOPE + 1]).unwrap();
        assert_eq!(refused(env.run(&Fake::default())), "io");
    }

    /// Held for the whole run: the service can't swap `<data>\updates` (or
    /// `<data>`) for a junction once the helper has checked it.
    #[cfg(windows)]
    #[test]
    fn the_data_folders_are_pinned_while_it_runs() {
        struct Swapper {
            fake: Fake,
            updates: PathBuf,
            data: PathBuf,
            renamed: Mutex<Vec<bool>>,
        }
        impl System for Swapper {
            fn check_data(&self, d: &Path) -> Result<(), String> {
                self.fake.check_data(d)
            }
            fn secure_staging(&self, d: &Path) -> Result<(), String> {
                self.fake.secure_staging(d)
            }
            fn authenticode(&self, m: &Path) -> Result<(), String> {
                self.fake.authenticode(m)
            }
            fn stop_service(&self) -> io::Result<()> {
                self.fake.stop_service()
            }
            fn start_service(&self) {
                self.fake.start_service()
            }
            fn msiexec(&self, a: &[OsString]) -> io::Result<i32> {
                self.fake.msiexec(a)
            }
            fn healthy(&self) -> bool {
                true
            }
            fn set_start(&self, m: Start) -> io::Result<()> {
                self.fake.set_start(m)
            }
            fn start_type(&self) -> io::Result<Start> {
                self.fake.start_type()
            }
            fn event_log(&self, m: &str) {
                self.fake.event_log(m)
            }
            fn service_processes(&self) -> io::Result<usize> {
                self.fake.service_processes()
            }
            fn copied(&self, _: &str) {
                let moved = self.updates.with_file_name("moved");
                let mut r = self.renamed.lock().unwrap();
                r.push(std::fs::rename(&self.updates, &moved).is_ok());
                r.push(std::fs::rename(&self.data, self.data.with_file_name("moved-data")).is_ok());
            }
        }
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let sys = Swapper {
            fake: Fake::default(),
            updates: env.ctx.updates(),
            data: env.ctx.data_dir.clone(),
            renamed: Mutex::default(),
        };
        assert_eq!(run(&env.ctx, &sys).unwrap().code(), "installed");
        assert!(sys.renamed.lock().unwrap().iter().all(|r| !r), "pinned");
    }

    #[test]
    fn a_failed_install_is_skipped_and_the_old_service_started() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            code: 1603,
            ..Fake::default()
        };
        assert_eq!(
            env.run(&fake).unwrap(),
            Outcome::Failed {
                version: Version::new(0, 7, 0),
                code: 1603
            }
        );
        assert_eq!(fake.calls().last().unwrap(), "start");
        let skipped: Value = serde_json::from_slice(
            &std::fs::read(env.ctx.data_dir.join("bin").join("skipped")).unwrap(),
        )
        .unwrap();
        assert_eq!(skipped, json!([{"version": "0.7.0", "reason": "failed"}]));
        assert!(!env.ctx.staging.current().exists());
    }

    /// After exit 3010 probation waits for the restart: the journal
    /// stays, a run before the restart (the old program still on disk)
    /// judges nothing, and the first run after it (the new one) does.
    #[test]
    fn a_restart_defers_probation_until_after_it() {
        let mut env = Env::new("0.6.0");
        env.offer("0.7.0");
        let out = env
            .run(&Fake {
                code: 3010,
                ..Fake::default()
            })
            .unwrap();
        assert_eq!(
            out,
            Outcome::Installed {
                version: Version::new(0, 7, 0),
                restart: true
            }
        );
        let journal = env.ctx.staging.dir.join(rollback::JOURNAL);
        assert!(journal.exists());
        // Before the restart: nothing judged, nothing started.
        let early = Fake {
            unhealthy: true,
            ..Fake::default()
        };
        assert_eq!(env.run(&early).unwrap().code(), "installed");
        assert!(early.calls().is_empty());
        assert!(journal.exists());
        // A planted answer is display only: it decides nothing.
        std::fs::write(
            env.updates(msi::RESULT),
            br#"{"format":1,"id":"0000000000000000","version":"0.7.0","outcome":"rollback_halted","at":0}"#,
        )
        .unwrap();
        // After the restart the program on disk is 0.7.0.
        env.ctx.running = Some(Version::new(0, 7, 0));
        let boot = Fake::default();
        assert_eq!(
            env.run(&boot).unwrap(),
            Outcome::Installed {
                version: Version::new(0, 7, 0),
                restart: false
            }
        );
        assert!(!journal.exists());
        assert!(
            boot.ran.lock().unwrap().is_none(),
            "nothing installed again"
        );
    }

    /// The MSI the helper installed before 0.7.0 (0.6.0), kept in
    /// `current\` as an earlier run leaves it, and a store.
    fn with_previous(env: &Env, msi: &[u8]) {
        let current = env.ctx.staging.current();
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("lodestar-0.6.0.msi"), msi).unwrap();
        let pins = Pins {
            version: "0.6.0".into(),
            channel: Channel::Stable,
            sha256: Sha256::of(msi).to_hex(),
            size: msi.len() as u64,
        };
        std::fs::write(current.join(PINS), serde_json::to_vec(&pins).unwrap()).unwrap();
        std::fs::write(env.ctx.data_dir.join("lodestar.db"), b"old store").unwrap();
    }

    const OLD_MSI: &[u8] = b"the 0.6.0 msi";

    /// Every call of a whole rollback after a failed probation, in order.
    const ROLLBACK: [&str; 16] = [
        "copied manifest.json",
        "copied lodestar-0.7.0.msi",
        "stop",
        "msiexec",
        // Step 1: disabled, stopped, marker and skip.
        "disabled",
        "stop",
        // Step 2: the snapshot chosen.
        "copied chosen",
        // Step 3: stopped again, the store restored.
        "disabled",
        "stop",
        "copied sides_removed",
        // Step 4: stopped again, the previous MSI.
        "disabled",
        "stop",
        "msiexec",
        // Step 5.
        "auto",
        "start",
        "copied rotated",
    ];

    fn failing(env: &Env) -> Fake {
        Fake {
            unhealthy: true,
            migrate: Some(env.ctx.data_dir.clone()),
            ..Fake::default()
        }
    }

    /// Probation fails: the store is restored before the previous MSI is
    /// reinstalled, with the service disabled meanwhile and the marker
    /// keeping every store opener out while the store is restored.
    #[test]
    fn a_failed_probation_restores_the_store_then_the_previous_msi() {
        let env = Env::new("0.6.0");
        with_previous(&env, OLD_MSI);
        env.offer("0.7.0");
        let marker = env.ctx.data_dir.join("bin").join(rollback::MARKER);
        let seen_marker = std::sync::Arc::new(Mutex::new(None));
        let (m, seen) = (marker.clone(), seen_marker.clone());
        let fake = Fake {
            at_hook: Some((
                "chosen",
                Box::new(move || *seen.lock().unwrap() = Some(m.exists())),
            )),
            ..failing(&env)
        };
        assert_eq!(
            env.run(&fake).unwrap(),
            Outcome::RolledBack {
                failed: "0.7.0".into(),
                to: "0.6.0".into(),
                healthy: true
            }
        );
        assert_eq!(fake.calls(), ROLLBACK);
        assert_eq!(
            *seen_marker.lock().unwrap(),
            Some(true),
            "marker during the restore"
        );
        assert!(!marker.exists(), "and gone after it");
        // The store was the old one again before the old MSI ran.
        let seen = fake.store_seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            [Some(b"old store".to_vec()), Some(b"old store".to_vec())]
        );
        assert_eq!(fake.ran.lock().unwrap().clone().unwrap().1, OLD_MSI);
        let args = fake.ran.lock().unwrap().clone().unwrap().0;
        assert!(args[5].to_string_lossy().ends_with("msiexec-rollback.log"));
        let data = &env.ctx.data_dir;
        assert_eq!(
            std::fs::read(data.join("lodestar.db")).unwrap(),
            b"old store"
        );
        assert!(!data.join("lodestar.pre-v5.db").exists());
        // The failed version is skipped; 0.6.0's MSI is current again.
        let skipped: Value =
            serde_json::from_slice(&std::fs::read(data.join("bin").join("skipped")).unwrap())
                .unwrap();
        assert_eq!(skipped, json!([{"version": "0.7.0", "reason": "failed"}]));
        let s = &env.ctx.staging;
        assert!(s.current().join("lodestar-0.6.0.msi").exists());
        assert!(!s.previous().exists());
        assert!(!s.dir.join(rollback::JOURNAL).exists());
        assert_eq!(env.answer().outcome, "rolled_back");
    }

    /// A crash at any call of the rollback, a step's hooks included
    /// (after the snapshot is chosen, inside the restore, after the last
    /// rotation), is finished by the next run.
    #[test]
    fn an_interrupted_rollback_is_finished_by_the_next_run() {
        for (crash, call) in ROLLBACK.iter().enumerate().skip(4) {
            let mut env = Env::new("0.6.0");
            with_previous(&env, OLD_MSI);
            env.offer("0.7.0");
            let data = env.ctx.data_dir.clone();
            let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                env.run(&Fake {
                    crash_at: Some(crash),
                    ..failing(&env)
                })
            }));
            assert!(crashed.is_err(), "crash {crash}");
            assert!(
                env.ctx.staging.dir.join(rollback::JOURNAL).exists(),
                "crash {crash}"
            );
            // The boot run, with 0.7.0 on disk: the journal first, the
            // request never again. The journal says "rolling back" before
            // the first change, so no crash re-judges probation.
            env.ctx.running = Some(Version::new(0, 7, 0));
            let boot = Fake {
                unhealthy: false,
                ..Fake::default()
            };
            let out = env.run(&boot).unwrap();
            assert_eq!(
                out,
                Outcome::RolledBack {
                    failed: "0.7.0".into(),
                    to: "0.6.0".into(),
                    healthy: true
                },
                "crash {crash} ({call})"
            );
            assert_eq!(
                std::fs::read(data.join("lodestar.db")).unwrap(),
                b"old store"
            );
            assert!(
                !data.join("bin").join(rollback::MARKER).exists(),
                "crash {crash}"
            );
            assert!(!env.ctx.staging.dir.join(rollback::JOURNAL).exists());
            assert!(
                env.ctx
                    .staging
                    .current()
                    .join("lodestar-0.6.0.msi")
                    .exists()
            );
            // The old MSI runs again only if its step wasn't recorded done
            // (call 12 is that msiexec); never twice after it.
            let reran = boot.ran.lock().unwrap().clone().map(|r| r.1);
            if crash <= 12 {
                assert_eq!(reran.as_deref(), Some(OLD_MSI), "crash {crash}");
            } else {
                assert_eq!(reran, None, "crash {crash}");
            }
            assert_eq!(env.run(&Fake::default()).unwrap(), Outcome::Nothing);
        }
    }

    /// A crash during the install: the next run tells from the program on
    /// disk whether it happened.
    #[test]
    fn a_crash_during_the_install_is_judged_by_the_next_run() {
        for installed in [false, true] {
            let mut env = Env::new("0.6.0");
            with_previous(&env, OLD_MSI);
            env.offer("0.7.0");
            // Call 3 is the install's msiexec.
            let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                env.run(&Fake {
                    crash_at: Some(3),
                    ..Fake::default()
                })
            }));
            assert!(crashed.is_err());
            if installed {
                env.ctx.running = Some(Version::new(0, 7, 0));
                let next = Fake::default();
                assert_eq!(env.run(&next).unwrap().code(), "installed");
                assert_eq!(next.calls()[..2], ["auto", "start"]);
            } else {
                let next = Fake::default();
                assert_eq!(
                    env.run(&next).unwrap(),
                    Outcome::InstallInterrupted {
                        version: "0.7.0".into()
                    }
                );
                assert_eq!(next.calls(), ["auto", "start"]);
            }
            assert!(!env.ctx.staging.dir.join(rollback::JOURNAL).exists());
        }
    }

    /// Someone installed another version by hand: the journal is over.
    #[test]
    fn a_manual_install_drops_the_journal() {
        let mut env = Env::new("0.6.0");
        env.offer("0.7.0");
        env.run(&Fake {
            code: 3010,
            ..Fake::default()
        })
        .unwrap();
        env.ctx.running = Some(Version::new(0, 8, 0));
        let next = Fake::default();
        assert_eq!(env.run(&next).unwrap(), Outcome::Nothing);
        assert!(!env.ctx.staging.dir.join(rollback::JOURNAL).exists());
        assert!(next.calls().is_empty());
    }

    /// A previous MSI that doesn't match its pins, wrong pins, none, or
    /// only one planted in the data folder: halted before anything is
    /// changed (the service left as it is), reported, and the journal
    /// dropped so later updates still run.
    #[test]
    fn a_missing_or_tampered_previous_msi_halts_before_anything_changes() {
        for tamper in ["bytes", "pins", "missing", "planted"] {
            let env = Env::new("0.6.0");
            if tamper == "planted" {
                // No kept MSI; one in the data folder is never used.
                std::fs::write(env.ctx.data_dir.join("lodestar.db"), b"old store").unwrap();
                std::fs::write(env.updates("lodestar-0.6.0.msi"), OLD_MSI).unwrap();
            } else {
                with_previous(&env, OLD_MSI);
            }
            let prev = env.ctx.staging.current();
            match tamper {
                "bytes" => std::fs::write(prev.join("lodestar-0.6.0.msi"), b"evil").unwrap(),
                "pins" => std::fs::write(
                    prev.join(PINS),
                    br#"{"version":"0.6.0","channel":"stable","sha256":"00","size":1}"#,
                )
                .unwrap(),
                "missing" => std::fs::remove_dir_all(&prev).unwrap(),
                _ => {}
            }
            env.offer("0.7.0");
            let fake = failing(&env);
            let out = env.run(&fake).unwrap();
            assert_eq!(out.code(), "rollback_halted", "{tamper}");
            let calls = fake.calls();
            assert_eq!(
                &calls[calls.len() - 2..],
                ["msiexec", "event"],
                "{tamper}: {calls:?}"
            );
            assert!(
                fake.events.lock().unwrap()[0].contains("Nothing was changed"),
                "{tamper}"
            );
            assert_eq!(
                calls.iter().filter(|c| *c == "msiexec").count(),
                1,
                "{tamper}"
            );
            assert_eq!(
                std::fs::read(env.ctx.data_dir.join("lodestar.db")).unwrap(),
                b"migrated store",
                "{tamper}"
            );
            // Nothing to resume: the journal is gone, later updates run.
            assert!(
                !env.ctx.staging.dir.join(rollback::JOURNAL).exists(),
                "{tamper}"
            );
            let again = Fake::default();
            assert_eq!(env.run(&again).unwrap(), Outcome::Nothing);
            assert!(again.calls().is_empty());
        }
    }

    /// A halt once the service was stopped leaves it stopped on manual
    /// start: neither version starts (§12.2).
    #[test]
    fn a_halt_after_the_stop_starts_neither_version() {
        // The kept MSI tampered once the store is being restored.
        let env = Env::new("0.6.0");
        with_previous(&env, OLD_MSI);
        env.offer("0.7.0");
        let kept = env.ctx.staging.previous().join("lodestar-0.6.0.msi");
        let fake = Fake {
            at_hook: Some((
                "sides_removed",
                Box::new(move || std::fs::write(&kept, b"evil").unwrap()),
            )),
            ..failing(&env)
        };
        assert_eq!(env.run(&fake).unwrap().code(), "rollback_halted");
        let calls = fake.calls();
        // The halt logs, then makes sure nothing runs: disabled, stopped.
        assert_eq!(
            &calls[calls.len() - 3..],
            ["event", "disabled", "stop"],
            "{calls:?}"
        );
        assert!(!calls[4..].contains(&"start".to_string()), "{calls:?}");
        assert!(!calls[4..].contains(&"manual".to_string()), "{calls:?}");
        assert!(!calls[4..].contains(&"auto".to_string()), "{calls:?}");
        // How to recover, in the Event Log and in result.json alike.
        let event = fake.events.lock().unwrap()[0].clone();
        for needle in [
            "step 3 of 5",
            "sc config Lodestar start= delayed-auto",
            "journal.json",
            "Neither file starts anything",
        ] {
            assert!(event.contains(needle), "{needle}: {event}");
        }
        assert_eq!(env.answer().detail.as_deref(), Some(event.as_str()));
        // The store was restored, the marker went with it.
        let data = &env.ctx.data_dir;
        assert_eq!(
            std::fs::read(data.join("lodestar.db")).unwrap(),
            b"old store"
        );
        assert!(!data.join("bin").join(rollback::MARKER).exists());
    }

    /// A completed rollback puts back the start type the service had
    /// before it, not delayed automatic by default (runner, 2026-11-09).
    #[test]
    fn a_rollback_restores_the_start_type_it_found() {
        for (before, note) in [
            (Start::Manual, "manual"),
            (Start::Immediate, "immediate"),
            (Start::Auto, "auto"),
        ] {
            let env = Env::new("0.6.0");
            with_previous(&env, OLD_MSI);
            env.offer("0.7.0");
            let fake = Fake {
                start_before: Some(before),
                ..failing(&env)
            };
            assert_eq!(env.run(&fake).unwrap().code(), "rolled_back");
            let calls = fake.calls();
            assert_eq!(calls[calls.len() - 3], note, "{calls:?}");
            assert_eq!(calls.iter().filter(|c| *c == "disabled").count(), 3);
        }
    }

    /// A snapshot changed after it was chosen halts before the restore:
    /// the store stays, the service stopped, the marker in place.
    #[test]
    fn a_snapshot_changed_since_it_was_chosen_halts() {
        let env = Env::new("0.6.0");
        with_previous(&env, OLD_MSI);
        env.offer("0.7.0");
        let snap = env.ctx.data_dir.join("lodestar.pre-v5.db");
        let fake = Fake {
            at_hook: Some((
                "chosen",
                Box::new(move || std::fs::write(&snap, b"other").unwrap()),
            )),
            ..failing(&env)
        };
        let out = env.run(&fake).unwrap();
        assert!(
            matches!(&out, Outcome::RollbackHalted { why, .. } if why.contains("changed")),
            "{out:?}"
        );
        let data = &env.ctx.data_dir;
        assert_eq!(
            std::fs::read(data.join("lodestar.db")).unwrap(),
            b"migrated store"
        );
        assert!(data.join("bin").join(rollback::MARKER).exists());
        let calls = fake.calls();
        assert_eq!(&calls[calls.len() - 3..], ["event", "disabled", "stop"]);
        let event = fake.events.lock().unwrap()[0].clone();
        assert!(
            event.contains("half restored") && event.contains("rollback.json"),
            "{event}"
        );
    }

    /// A snapshot with its own journal beside it was mid-write: never
    /// restored (SYSTEM doesn't open SQLite to settle it).
    #[test]
    fn a_snapshot_mid_write_isnt_restored() {
        let env = Env::new("0.6.0");
        with_previous(&env, OLD_MSI);
        env.offer("0.7.0");
        let side = env.ctx.data_dir.join("lodestar.pre-v5.db-journal");
        let fake = Fake {
            at_hook: Some((
                "chosen",
                Box::new(move || std::fs::write(&side, b"hot").unwrap()),
            )),
            ..failing(&env)
        };
        let out = env.run(&fake).unwrap();
        assert!(
            matches!(&out, Outcome::RollbackHalted { why, .. } if why.contains("beside it")),
            "{out:?}"
        );
        assert_eq!(
            std::fs::read(env.ctx.data_dir.join("lodestar.db")).unwrap(),
            b"migrated store"
        );
    }

    /// A snapshot that was there before the install is never restored.
    #[test]
    fn only_the_failed_versions_snapshot_is_restored() {
        let env = Env::new("0.6.0");
        with_previous(&env, OLD_MSI);
        let older = env.ctx.data_dir.join("lodestar.pre-v3.db");
        std::fs::write(&older, b"months old").unwrap();
        env.offer("0.7.0");
        assert_eq!(env.run(&failing(&env)).unwrap().code(), "rolled_back");
        assert_eq!(
            std::fs::read(env.ctx.data_dir.join("lodestar.db")).unwrap(),
            b"old store"
        );
        assert_eq!(std::fs::read(&older).unwrap(), b"months old");
    }

    /// A journal naming a snapshot that isn't one beside the store halts.
    #[test]
    fn a_snapshot_name_that_isnt_beside_the_store_halts() {
        let env = Env::new("0.6.0");
        with_previous(&env, OLD_MSI);
        std::fs::create_dir_all(&env.ctx.staging.dir).unwrap();
        for name in ["../lodestar.pre-v5.db", "other.pre-v5.db", "lodestar.db"] {
            let j = rollback::Journal {
                format: 1,
                id: "0123456789abcdef".into(),
                from: "0.6.0".into(),
                to: "0.7.0".into(),
                snapshots_before: vec![],
                restart: false,
                start_before: None,
                state: rollback::State::RollingBack {
                    snapshot: Some(rollback::SnapshotRef {
                        name: name.into(),
                        sha256: "00".into(),
                    }),
                    done: 2,
                },
            };
            rollback::write(&env.ctx, &j).unwrap();
            let out = env.run(&Fake::default()).unwrap();
            assert!(
                matches!(&out, Outcome::RollbackHalted { why, .. } if why.contains("beside the store")),
                "{name}: {out:?}"
            );
        }
    }

    /// Something still running as the service's account blocks the
    /// install (#640's security review).
    #[test]
    fn a_process_left_as_the_service_blocks_the_install() {
        let env = Env::new("0.6.0");
        env.offer("0.7.0");
        let fake = Fake {
            processes: 1,
            ..Fake::default()
        };
        assert_eq!(refused(env.run(&fake)), "service_processes");
        assert!(fake.ran.lock().unwrap().is_none());
        assert_eq!(fake.calls().last().unwrap(), "start");
    }

    /// On a first install the helper has seen no manifest; the data
    /// folder's verified marks keep a replayed older one out.
    #[test]
    fn the_data_folders_marks_raise_the_floor_on_a_first_install() {
        let env = Env::new("0.6.0");
        // The service saw a newer manifest.
        let newer = env.envelope(Channel::Stable, NOW - 60, "0.7.1", MSI);
        let bin = env.ctx.data_dir.join("bin");
        let loaded = trust_file::load(&bin, &env.ctx.embedded, &[]);
        let c = lodestar_update::check(
            &newer,
            Channel::Stable,
            NOW,
            &env.ctx.embedded,
            loaded.state(),
        )
        .unwrap();
        trust_file::save(&bin, &loaded, c.next).unwrap();
        // A replayed older, still current manifest.
        let old = env.envelope(Channel::Stable, NOW - 3600, "0.7.0", MSI);
        env.leave("0.7.0", Channel::Stable, &old, MSI);
        let fake = Fake::default();
        assert_eq!(refused(env.run(&fake)), "older_than_mark");
        assert!(fake.ran.lock().unwrap().is_none());
    }

    /// After a key rotation the floor still holds: the data folder's
    /// statements come with its marks, so the rotation in a replayed
    /// manifest doesn't reset them (#640's security review, item 2).
    #[test]
    fn the_floor_survives_a_key_rotation() {
        use lodestar_update::Role;
        use lodestar_update::testing::{TestKey, statement};
        let env = Env::new("0.6.0");
        let k2 = TestKey::new(9);
        let s = statement(
            &env.trust.recovery,
            1,
            Role::Release,
            &k2.public(),
            NOW - 7200,
        );
        let signed = |ts: u64, version: &str| {
            let mut m = manifest(Channel::Stable, ts, version, b"x");
            m["statements"] = json!([serde_json::to_value(&s).unwrap()]);
            m["releases"][0]["artifacts"] = json!([{
                "target": "x86_64-pc-windows-msvc",
                "kind": "msi",
                "url": format!("https://github.com/dvortsis/lodestar-v2/releases/download/v{version}/l.msi"),
                "sha256": Sha256::of(MSI).to_hex(),
                "size": MSI.len(),
            }]);
            envelope(&k2.signed(&bytes(&m)), None)
        };
        // The service saw the newer manifest, after the rotation.
        let bin = env.ctx.data_dir.join("bin");
        let loaded = trust_file::load(&bin, &env.ctx.embedded, &[]);
        let c = lodestar_update::check(
            &signed(NOW - 60, "0.7.1"),
            Channel::Stable,
            NOW,
            &env.ctx.embedded,
            loaded.state(),
        )
        .unwrap();
        trust_file::save(&bin, &loaded, c.next).unwrap();
        // Replayed: an older one, after the rotation too, still current.
        env.leave("0.7.0", Channel::Stable, &signed(NOW - 3600, "0.7.0"), MSI);
        let fake = Fake::default();
        assert_eq!(refused(env.run(&fake)), "older_than_mark");
        assert!(fake.ran.lock().unwrap().is_none());
    }

    /// The data folder's revocations never reach the helper's own state,
    /// and a damaged `trust.json` there is ignored.
    #[test]
    fn the_data_folder_raises_only_marks() {
        let env = Env::new("0.6.0");
        let bin = env.ctx.data_dir.join("bin");
        let loaded = trust_file::load(&bin, &env.ctx.embedded, &[]);
        let mut padded = loaded.state().clone();
        let key = env.trust.nightly.public().to_base64();
        padded.revoked.push(key.clone());
        std::fs::write(bin.join("trust.json"), padded.to_bytes().unwrap()).unwrap();
        env.offer("0.7.0");
        assert_eq!(env.run(&Fake::default()).unwrap().code(), "installed");
        let own = std::fs::read_to_string(env.ctx.staging.dir.join("trust.json")).unwrap();
        assert!(!own.contains(&key), "{own}");
        // Damaged: ignored, not a refusal.
        let env = Env::new("0.6.0");
        std::fs::write(
            env.ctx.data_dir.join("bin").join("trust.json"),
            b"{nonsense",
        )
        .unwrap();
        env.offer("0.7.0");
        assert_eq!(env.run(&Fake::default()).unwrap().code(), "installed");
    }

    #[test]
    fn the_context_needs_the_install_folder() {
        let ok = Context::new(
            Path::new(r"C:\Program Files\Lodestar\lodestar.exe"),
            Some(Path::new(r"C:\Program Files")),
            Some(r"C:\ProgramData".into()),
            "0.6.0",
            None,
            NOW,
            TestTrust::default().embedded(vec![]),
        )
        .unwrap();
        assert_eq!(ok.data_dir, Path::new(r"C:\ProgramData").join("Lodestar"));
        assert_eq!(ok.running, Some(Version::new(0, 6, 0)));
        // Windows paths only split on Windows.
        #[cfg(windows)]
        assert_eq!(
            ok.staging.dir,
            Path::new(r"C:\Program Files\Lodestar").join("updates")
        );
        for (exe, pf) in [
            (r"C:\Users\x\lodestar.exe", Some(r"C:\Program Files")),
            (r"C:\Program Files\Lodestar\lodestar.exe", None),
            (
                r"C:\Program Files\Other\lodestar.exe",
                Some(r"C:\Program Files"),
            ),
        ] {
            let r = Context::new(
                Path::new(exe),
                pf.map(Path::new),
                None,
                "0.6.0",
                None,
                NOW,
                TestTrust::default().embedded(vec![]),
            );
            assert_eq!(r.unwrap_err(), Refusal::NotInstalled, "{exe}");
        }
    }

    #[test]
    fn reads_sc_and_authenticode() {
        let q = "\r\nSERVICE_NAME: Lodestar\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        STATE              : 4  RUNNING\r\n";
        assert_eq!(service_state(q).as_deref(), Some("RUNNING"));
        assert_eq!(service_state("nothing"), None);
        let qc = |t: &str| {
            start_type_of(&format!(
                "SERVICE_NAME: Lodestar\r\n        START_TYPE         : {t}\r\n"
            ))
        };
        assert_eq!(qc("2   AUTO_START  (DELAYED)"), Some(Start::Auto));
        assert_eq!(qc("2   AUTO_START"), Some(Start::Immediate));
        assert_eq!(qc("3   DEMAND_START"), Some(Start::Manual));
        assert_eq!(qc("4   DISABLED"), Some(Start::Disabled));
        assert_eq!(start_type_of("nothing"), None);
        let rows = "\"lodestar.exe\",\"4120\",\"Services\",\"0\",\"52,000 K\"\r\n";
        assert_eq!(count_rows(rows), 1);
        assert_eq!(
            count_rows("INFO: No tasks are running which match the specified criteria.\r\n"),
            0
        );
        assert!(authenticode_verdict("NotSigned\r\n").is_ok());
        assert!(authenticode_verdict("Valid").is_ok());
        assert!(authenticode_verdict("HashMismatch").is_err());
        assert!(authenticode_verdict("").is_err());
    }

    #[test]
    fn children_get_no_lodestar_variables() {
        let mut cmd = std::process::Command::new("x");
        let names = [
            "LODESTAR_RELAUNCH_TOKEN",
            "lodestar_data_dir",
            "PATH",
            "SystemRoot",
        ];
        clean_env_of(&mut cmd, names.iter().map(OsString::from));
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(removed.len(), 2, "{removed:?}");
        assert!(
            removed
                .iter()
                .all(|k| k.to_ascii_uppercase().starts_with("LODESTAR_"))
        );
    }

    /// The same `tasklist` filter for real: SYSTEM always runs processes,
    /// so a filter that silently matched nothing would show here.
    #[cfg(windows)]
    #[test]
    fn tasklist_counts_an_accounts_processes_for_real() {
        let system = Windows::processes_of("NT AUTHORITY\\SYSTEM").unwrap();
        assert!(system >= 1, "{system}");
        assert_eq!(
            Windows::processes_of("NT SERVICE\\LodestarNoSuchService").unwrap(),
            0
        );
    }

    /// The staging folder's ACL for real: created private to SYSTEM and
    /// Administrators, and a folder that isn't is refused.
    #[cfg(windows)]
    #[test]
    fn staging_is_private_for_real() {
        if std::env::var_os("CI").is_none() && std::env::var_os("LODESTAR_SERVICE_TEST").is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("updates");
        Windows.secure_staging(&dir).unwrap();
        let seen = {
            use crate::windows::acl::Ops;
            crate::windows::acl::Windows { own_sid: None }
                .inspect(&dir)
                .unwrap()
                .unwrap()
        };
        let sddl = crate::windows::acl::parse_sddl(&seen.folder.sddl).unwrap();
        // Protected, SYSTEM and Administrators only: the production rule.
        assert!(sddl.dacl.starts_with('P'), "{sddl:?}");
        assert_eq!(
            crate::windows::acl::check_dacl(&sddl.dacl, None),
            Ok(()),
            "{sddl:?}"
        );
        assert!(!sddl.dacl.contains("S-1-5-80-"), "{sddl:?}");
        Windows.secure_staging(&dir).unwrap();
        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        assert!(Windows.secure_staging(&open).is_err());
    }
}
