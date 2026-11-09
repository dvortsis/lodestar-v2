//! The MSI's probation and rollback (PK-win-upd-3; architecture §12.2,
//! plan Q13 rule 2, B-update-r2's store-before-MSI order), run by the
//! update helper as SYSTEM.
//!
//! **The journal** is `journal.json` in the helper's staging folder, which
//! only SYSTEM and Administrators can write: nothing in the data folder
//! decides a rollback (`result.json` there is display only). It's written
//! by temporary file and rename after each step. On Windows that rename
//! isn't write-through, so a power cut can bring back the step before;
//! every step is idempotent, so repeating it is safe. A crash or restart
//! at any point is finished by the next run of the helper, at boot (the
//! task's trigger) or on demand ([`resume`]), before it reads any request.
//!
//! - **Installing:** written before `msiexec` runs. Probation follows: the
//!   new service must reach Running and stay there
//!   ([`super::helper::System::healthy`]). Passed, the journal goes. The
//!   next run tells what happened from the helper's own version (the
//!   program on disk, a SYSTEM-owned fact): still `from` means the install
//!   never happened, or waits for a restart (exit 3010); `to` means judge
//!   probation now; anything else means someone installed another version
//!   by hand, and the journal is dropped.
//! - **Rolling back** (a failed probation), each step idempotent:
//!   1. the service's start type set to **disabled** (the SCM's failure
//!      actions restart a crashed service even on manual start), the
//!      service stopped, nothing left running as its account, then under
//!      `.lock` the marker `<data>\bin\rollback.json` written (every store
//!      opener, `serve` included, refuses while it exists; the helper
//!      never reads it) and the failed version marked `failed` in
//!      `skipped`;
//!   2. the snapshot to restore chosen by name alone: the earliest
//!      pre-migration snapshot the failed version took (one not there
//!      before the install); its SHA-256 recorded. SYSTEM never opens the
//!      store;
//!   3. **the store first:** the snapshot renamed over `lodestar.db` (its
//!      `-wal`, `-shm` and `-journal` removed) beneath the pinned data
//!      folder, then the marker removed. A repeat that finds the snapshot
//!      gone and the store at its SHA-256 counts it done;
//!   4. **then the previous MSI,** found by its pins (version `from`, size,
//!      SHA-256) in staging's `previous\` or `current\`, checked right
//!      before it runs (rule 4's one exception: the journal's rollback
//!      target with SYSTEM-held pins);
//!   5. the start type back to what it was before (recorded before the
//!      first change), the service started, the
//!      reinstalled MSI made `current\` again; then the old version's
//!      health is checked and reported.
//!
//!   Steps 1, 3 and 4 stop the service and check for its processes on
//!   every pass, so a resume never finds it running.
//! - **Stops:** a transient one (the service won't stop yet, a process
//!   lingers, Windows Installer is busy) leaves the journal, the service
//!   disabled, for the next run to retry ([`Outcome::RollbackPending`]). A
//!   final one halts: as §12.2 says, neither version starts. Before step 1
//!   nothing was changed and the service is left as it is; after it, the
//!   service stays stopped and disabled (with the marker while the store
//!   isn't restored). The journal, the Event Log and `result.json` say
//!   which step stopped and how an administrator recovers ([`recovery`]).

use std::io::Read;
use std::path::{Path, PathBuf};

use lodestar_store::snapshots::{self, Kind as SnapKind};
use lodestar_update::Sha256;
use lodestar_update::semver::Version;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::helper::{self, Context, Outcome, Start, System};
use super::msi::{self, PINS, Pins, Refusal};
use crate::update::layout::{self, Bin};

/// In staging.
pub const JOURNAL: &str = "journal.json";
/// In `<data>\bin`: every store opener refuses while it exists
/// (`cli::util::refuse_during_update_rollback`). Never read here.
pub const MARKER: &str = "rollback.json";
const FORMAT: u32 = 1;
/// The store, in the data folder (the default `Config::db_path`), and the
/// stem its snapshots are named by.
const STORE: &str = "lodestar.db";
const STEM: &str = "lodestar";
/// The reinstall's own log, so the failed install's stays.
const ROLLBACK_LOG: &str = "msiexec-rollback.log";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub format: u32,
    /// The request it came from.
    pub id: String,
    /// The version installed from (this build, when it was written).
    pub from: String,
    /// The version installed.
    pub to: String,
    /// The store's snapshots before the install (names).
    pub snapshots_before: Vec<String>,
    /// The install needs Windows to restart (exit 3010).
    #[serde(default)]
    pub restart: bool,
    /// The service's start type before the rollback disabled it: what
    /// it's set back to (recorded before the first change).
    #[serde(default)]
    pub start_before: Option<Start>,
    /// The `msiexec` run handed to the helper's copy, while it's pending:
    /// what the copy runs, and nothing else.
    #[serde(default)]
    pub msi: Option<MsiRun>,
    pub state: State,
}

/// What an `msiexec` run is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Install,
    Rollback,
}

/// An `msiexec` run the helper hands to its copy in staging, so the
/// install folder isn't in use while Windows Installer replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MsiRun {
    pub purpose: Purpose,
    /// The MSI, inside staging.
    pub msi: String,
    /// Its log, inside staging.
    pub log: String,
    /// What the MSI was checked against (checked again by the copy).
    pub pins: Pins,
}

/// In staging: the copy's answer, `msiexec`'s exit code.
pub const MSI_RESULT: &str = "msiexec-result.json";
/// In staging: held by the copy while it runs.
pub const MSI_LOCK: &str = "msiexec.lock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MsiResult {
    pub id: String,
    pub purpose: Purpose,
    pub code: i32,
}

/// The copy's exit code for this journal's pending run, consumed: read
/// and removed. `None` when there's none (yet).
pub fn take_result(ctx: &Context, j: &Journal) -> Option<i32> {
    let run = j.msi.as_ref()?;
    let path = ctx.staging.dir.join(MSI_RESULT);
    let r: MsiResult = layout::read_json(&path).ok().flatten()?;
    let _ = std::fs::remove_file(&path);
    (r.id == j.id && r.purpose == run.purpose).then_some(r.code)
}

pub fn write_result(ctx: &Context, r: &MsiResult) -> Result<(), Refusal> {
    let bytes = serde_json::to_vec(r).map_err(|e| Refusal::Io(e.to_string()))?;
    layout::write_atomic(&ctx.staging.dir, MSI_RESULT, &bytes)
        .map_err(|e| Refusal::Io(format!("{MSI_RESULT}: {e}")))
}

/// Takes the copy's lock in staging; `None` while a copy holds it.
pub fn msi_lock(ctx: &Context) -> std::io::Result<Option<std::fs::File>> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(ctx.staging.dir.join(MSI_LOCK))?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Whether a copy is running an `msiexec` now.
pub fn copy_running(ctx: &Context) -> bool {
    matches!(msi_lock(ctx), Ok(None))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "step", deny_unknown_fields)]
pub enum State {
    /// `msiexec` ran or is about to; probation is next.
    Installing,
    /// Rolling back: `done` is the last step finished (1 to 5).
    RollingBack {
        snapshot: Option<SnapshotRef>,
        done: u8,
    },
    /// It couldn't go on; nothing more is tried. `recovery` says what an
    /// administrator does.
    Halted {
        why: String,
        #[serde(default)]
        recovery: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRef {
    /// A file name beside the store, never a path.
    pub name: String,
    pub sha256: String,
}

/// The journal in staging, if any. One that can't be read refuses the
/// run rather than guess.
pub fn read(ctx: &Context) -> Result<Option<Journal>, Refusal> {
    match layout::read_json::<Journal>(&ctx.staging.dir.join(JOURNAL)) {
        Ok(Some(j)) if j.format == FORMAT => Ok(Some(j)),
        Ok(None) => Ok(None),
        _ => Err(Refusal::Io("journal.json can't be read".into())),
    }
}

pub fn write(ctx: &Context, j: &Journal) -> Result<(), Refusal> {
    let bytes = serde_json::to_vec_pretty(j).map_err(|e| Refusal::Io(e.to_string()))?;
    layout::write_atomic(&ctx.staging.dir, JOURNAL, &bytes)
        .map_err(|e| Refusal::Io(format!("journal.json: {e}")))
}

pub fn clear(ctx: &Context) {
    let _ = std::fs::remove_file(ctx.staging.dir.join(JOURNAL));
}

/// The journal written before `msiexec` runs. The store's snapshots must
/// be listed: without that list a rollback couldn't tell the failed
/// version's snapshot from an old one.
pub fn installing(
    ctx: &Context,
    id: &str,
    from: &Version,
    to: &Version,
) -> Result<Journal, Refusal> {
    let before = snapshots::list(&ctx.data_dir.join(STORE))
        .map_err(|e| Refusal::Io(format!("listing the store's snapshots: {e}")))?;
    Ok(Journal {
        format: FORMAT,
        id: id.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        snapshots_before: before.into_iter().map(|s| s.name).collect(),
        restart: false,
        start_before: None,
        msi: None,
        state: State::Installing,
    })
}

/// Probation for an installed version, then a rollback if it fails. The
/// journal is in place (`Installing`).
pub fn probation(ctx: &Context, sys: &dyn System, mut journal: Journal, restart: bool) -> Outcome {
    let Ok(to) = Version::parse(&journal.to) else {
        return halt(
            ctx,
            sys,
            journal,
            0,
            "the journal's version is invalid".into(),
        );
    };
    if restart {
        // The old file may run until Windows restarts: judged after it.
        journal.restart = true;
        let _ = write(ctx, &journal);
        return Outcome::Installed {
            version: to,
            restart: true,
        };
    }
    if sys.healthy() {
        clear(ctx);
        return Outcome::Installed {
            version: to,
            restart: false,
        };
    }
    roll_back(ctx, sys, journal)
}

/// Finishes what a journal left, before any request. `None` when there's
/// no journal, or it was dropped because another version was installed by
/// hand (the run then goes on to the request).
pub fn resume(
    ctx: &Context,
    sys: &dyn System,
    running: &Version,
) -> Result<Option<Outcome>, Refusal> {
    let Some(journal) = read(ctx)? else {
        return Ok(None);
    };
    let is = |v: &str| running.to_string() == v;
    if !is(&journal.from) && !is(&journal.to) {
        // Installed by hand since: whatever this journal was about is over.
        clear(ctx);
        let _ = remove_marker(ctx);
        return Ok(None);
    }
    // An install handed to the copy: its answer first, or it's still
    // running. Without either the copy died; the program on disk decides.
    if journal.state == State::Installing && journal.msi.is_some() {
        if let Some(code) = take_result(ctx, &journal) {
            return Ok(Some(helper::after_install(ctx, sys, journal, code)));
        }
        if copy_running(ctx) {
            return Ok(Some(Outcome::HandedOff));
        }
    }
    Ok(Some(match journal.state {
        State::Halted {
            ref why,
            ref recovery,
        } => Outcome::RollbackHalted {
            why: why.clone(),
            recovery: recovery.clone(),
        },
        State::RollingBack { .. } => roll_back(ctx, sys, journal),
        State::Installing if is(&journal.from) && journal.restart => {
            // Windows hasn't restarted yet: nothing to judge.
            Outcome::Installed {
                version: Version::parse(&journal.to).unwrap_or_else(|_| running.clone()),
                restart: true,
            }
        }
        State::Installing if is(&journal.from) => {
            // The install never finished: the old version is on disk.
            clear(ctx);
            let _ = sys.set_start(journal.start_before.unwrap_or(Start::Auto));
            sys.start_service();
            Outcome::InstallInterrupted {
                version: journal.to.clone(),
            }
        }
        State::Installing => {
            // A crash or restart may have left it stopped, or disabled by
            // a rollback's first step that wasn't recorded yet.
            let _ = sys.set_start(journal.start_before.unwrap_or(Start::Auto));
            sys.start_service();
            probation(ctx, sys, journal, false)
        }
    }))
}

/// Why the rollback stopped.
enum Stop {
    /// For the next run to retry; the service stays disabled.
    Retry(String),
    /// Final: nothing more is tried.
    Halt(String),
    /// The reinstall was handed to the helper's copy; the run that
    /// follows it finishes.
    HandedOff,
}

fn halt_on<T>(r: Result<T, String>) -> Result<T, Stop> {
    r.map_err(Stop::Halt)
}

/// The rollback, from wherever the journal stands.
fn roll_back(ctx: &Context, sys: &dyn System, mut j: Journal) -> Outcome {
    match steps(ctx, sys, &mut j) {
        Ok(()) => {
            let healthy = sys.healthy();
            clear(ctx);
            Outcome::RolledBack {
                failed: j.to.clone(),
                to: j.from.clone(),
                healthy,
            }
        }
        Err(Stop::Retry(why)) => Outcome::RollbackPending { why },
        Err(Stop::HandedOff) => Outcome::HandedOff,
        Err(Stop::Halt(why)) => {
            let done = done_of(&j);
            halt(ctx, sys, j, done, why)
        }
    }
}

fn done_of(j: &Journal) -> u8 {
    match &j.state {
        State::RollingBack { done, .. } => *done,
        _ => 0,
    }
}

/// A final stop. Neither version starts once the service was stopped
/// (§12.2): it stays stopped and disabled, with the marker in place
/// until the store is restored. Before that nothing was changed. The
/// journal, the Event Log and `result.json` say how to recover.
fn halt(ctx: &Context, sys: &dyn System, mut j: Journal, done: u8, why: String) -> Outcome {
    // `start_before` is recorded before the rollback first touches the
    // service: without it nothing was changed.
    let touched = j.start_before.is_some();
    let recovery = recovery(ctx, &j, if touched { done.max(1) } else { 0 }, &why);
    sys.event_log(&recovery);
    eprintln!("lodestar update-helper: {recovery}");
    if touched {
        // Neither version runs: whatever Windows Installer's own rollback
        // or the SCM may have started is stopped, and stays disabled.
        let _ = sys.set_start(Start::Disabled);
        let _ = sys.stop_service();
        j.state = State::Halted {
            why: why.clone(),
            recovery: recovery.clone(),
        };
        let _ = write(ctx, &j);
    } else {
        // Nothing to resume: the journal goes, so later updates still run.
        clear(ctx);
    }
    Outcome::RollbackHalted { why, recovery }
}

/// What an administrator does after a halt: which step stopped, and how
/// to bring Lodestar back. Neither the marker nor the journal starts
/// anything by itself; removing them is the administrator's step, once
/// the store is one the installed version reads.
pub fn recovery(ctx: &Context, j: &Journal, done: u8, why: &str) -> String {
    let (from, to) = (&j.from, &j.to);
    let journal = ctx.staging.dir.join(JOURNAL);
    if done == 0 {
        return format!(
            "Lodestar {to} failed its health check, and the rollback to {from} couldn't start: \
             {why}. Nothing was changed: {to} keeps running. To go back to {from}, an \
             administrator installs the {from} MSI by hand."
        );
    }
    let marker = ctx.data_dir.join("bin").join(MARKER);
    let store = if done < 3 {
        format!(
            "The store may be half restored, so {} stays and every store opener refuses \
             while it exists.",
            marker.display()
        )
    } else {
        format!("The store was restored to {from}'s; reinstalling {from} didn't finish.")
    };
    format!(
        "Lodestar {to} failed its health check, and the rollback to {from} stopped after step \
         {done} of 5: {why}. The Lodestar service is stopped and disabled, so neither version \
         opens the store. {store} To recover, an administrator: 1. reads {journal} and the \
         msiexec logs beside it; 2. installs the {from} MSI by hand, or restores a backup of \
         {data}; 3. once the store is one the installed version reads, deletes {marker} and \
         {journal}, then runs: sc config Lodestar start= delayed-auto, and: sc start Lodestar. \
         Installing a version other than {from} or {to} clears both files by itself. Neither \
         file starts anything.",
        journal = journal.display(),
        data = ctx.data_dir.display(),
        marker = marker.display(),
    )
}

fn steps(ctx: &Context, sys: &dyn System, j: &mut Journal) -> Result<(), Stop> {
    let (mut snapshot, mut done) = match &j.state {
        State::RollingBack { snapshot, done } => (snapshot.clone(), *done),
        _ => (None, 0),
    };
    let save = |j: &mut Journal, snapshot: &Option<SnapshotRef>, done: u8| {
        j.state = State::RollingBack {
            snapshot: snapshot.clone(),
            done,
        };
        write(ctx, j).map_err(|e| Stop::Halt(e.to_string()))
    };
    // The target first: without it nothing is touched.
    if done < 4 && j.msi.is_none() {
        halt_on(previous_msi(ctx, &j.from))?;
    }
    // The start type to go back to, recorded before the first change.
    if j.start_before.is_none() {
        let before = sys
            .start_type()
            .map_err(|e| Stop::Retry(format!("reading the service's start type: {e}")))?;
        j.start_before = Some(match before {
            // Only this helper disables it: never what it goes back to.
            Start::Disabled => Start::Auto,
            other => other,
        });
        save(j, &snapshot, done)?;
    }
    if done < 1 {
        stopped(sys)?;
        let bin = halt_on(
            Bin::existing(&ctx.data_dir)
                .map_err(|e| format!("bin: {e}"))
                .and_then(|b| b.ok_or_else(|| "bin is missing".to_string())),
        )?;
        let lock =
            helper::take_lock(&bin, ctx.lock_wait).map_err(|e| Stop::Retry(e.to_string()))?;
        halt_on(
            bin.write(MARKER, &serde_json::json!({ "windows_rollback": j.to }))
                .map_err(|e| format!("the rollback marker: {e}")),
        )?;
        halt_on(
            super::check::add_skip(&bin, &j.to, super::check::SKIP_FAILED)
                .map_err(|e| format!("skipped: {e}")),
        )?;
        drop(lock);
        done = 1;
        save(j, &snapshot, done)?;
    }
    if done < 2 {
        snapshot = halt_on(choose(ctx, &j.snapshots_before))?;
        sys.copied("chosen");
        done = 2;
        save(j, &snapshot, done)?;
    }
    if done < 3 {
        stopped(sys)?;
        if let Some(s) = &snapshot {
            halt_on(restore(ctx, sys, s))?;
        }
        if !remove_marker(ctx) {
            return Err(Stop::Retry(
                "the rollback marker couldn't be removed".into(),
            ));
        }
        done = 3;
        save(j, &snapshot, done)?;
    }
    if done < 4 {
        let code = match take_result(ctx, j) {
            Some(code) => code,
            None if copy_running(ctx) => return Err(Stop::HandedOff),
            None => {
                stopped(sys)?;
                // Checked again right before it runs.
                let (staged, pins) = halt_on(previous_msi(ctx, &j.from))?;
                let log = ctx.staging.dir.join(ROLLBACK_LOG);
                j.msi = Some(MsiRun {
                    purpose: Purpose::Rollback,
                    msi: staged.to_string_lossy().into_owned(),
                    log: log.to_string_lossy().into_owned(),
                    pins,
                });
                save(j, &snapshot, done)?;
                let args = msi::msiexec_args(&staged, &log);
                match sys.run_msiexec(&ctx.staging.dir, &args) {
                    Ok(helper::Ran::Code(c)) => c,
                    Ok(helper::Ran::HandedOff) => return Err(Stop::HandedOff),
                    // Never from Program Files instead: retried.
                    Err(why) => return Err(Stop::Retry(format!("the helper's copy: {why}"))),
                }
            }
        };
        j.msi = None;
        match msi::installed(code) {
            msi::Installed::Done | msi::Installed::RestartNeeded => {}
            msi::Installed::Busy(c) => {
                save(j, &snapshot, done)?;
                return Err(Stop::Retry(format!("Windows Installer is busy ({c})")));
            }
            msi::Installed::Failed(c) => {
                return Err(Stop::Halt(format!("reinstalling {} failed ({c})", j.from)));
            }
        }
        done = 4;
        save(j, &snapshot, done)?;
    }
    if done < 5 {
        let back = j.start_before.unwrap_or(Start::Auto);
        sys.set_start(back)
            .map_err(|e| Stop::Retry(format!("setting the service's start type back: {e}")))?;
        sys.start_service();
        rotate_back(ctx, &j.from);
        sys.copied("rotated");
        done = 5;
        save(j, &snapshot, done)?;
    }
    Ok(())
}

/// The service disabled and stopped, and nothing left running as its
/// account. Repeated before every step that changes the data folder or
/// runs an installer.
fn stopped(sys: &dyn System) -> Result<(), Stop> {
    sys.set_start(Start::Disabled)
        .map_err(|e| Stop::Retry(format!("disabling the service: {e}")))?;
    sys.stop_service()
        .map_err(|e| Stop::Retry(format!("the service didn't stop: {e}")))?;
    no_service_processes(sys).map_err(Stop::Retry)
}

/// Hardening (#640's security review): nothing may still run as the
/// service's account once it has stopped, before SYSTEM changes the data
/// folder or runs an installer whose custom actions walk it.
pub fn no_service_processes(sys: &dyn System) -> Result<(), String> {
    match sys.service_processes() {
        Ok(0) => Ok(()),
        Ok(n) => Err(format!(
            "{n} process(es) still run as the service's account"
        )),
        Err(e) => Err(format!("the service's processes can't be listed: {e}")),
    }
}

/// Under `.lock`: the marker goes once the store is restored (or the
/// journal was dropped). Whether it's gone.
fn remove_marker(ctx: &Context) -> bool {
    let marker = ctx.data_dir.join("bin").join(MARKER);
    if let Ok(Some(bin)) = Bin::existing(&ctx.data_dir)
        && let Ok(Some(_lock)) = bin.lock()
    {
        let _ = bin.remove(MARKER);
    }
    std::fs::symlink_metadata(marker).is_err()
}

/// The MSI for `from`, found by its pins in staging's `previous\` (as the
/// install leaves it) or `current\` (when the install was interrupted
/// before it rotated them), and checked against them.
fn previous_msi(ctx: &Context, from: &str) -> Result<(PathBuf, Pins), String> {
    let mut why = format!("no MSI for {from} is kept, so it can't be reinstalled");
    for dir in [ctx.staging.previous(), ctx.staging.current()] {
        let pins: Pins = match layout::read_json(&dir.join(PINS)) {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(e) => {
                why = format!("the kept pins can't be read: {e}");
                continue;
            }
        };
        if pins.version != from {
            continue;
        }
        let v = layout::parse_version(&pins.version).ok_or("the kept pins' version is invalid")?;
        let path = dir.join(msi::msi_name(&v));
        let (sha256, size) = hash(&path).map_err(|e| format!("the kept MSI: {e}"))?;
        if size != pins.size || sha256.to_hex() != pins.sha256 {
            return Err("the kept MSI doesn't match its pins".into());
        }
        return Ok((path, pins));
    }
    Err(why)
}

/// The snapshot to restore: the earliest pre-migration snapshot that
/// wasn't there before the install (the failed version's first
/// migration), or none when it took none (the store is kept). Chosen by
/// name alone: SYSTEM never opens the store, and a planted one gains
/// nothing (the service account can write the store itself, and the old
/// version checks it when it opens it).
fn choose(ctx: &Context, before: &[String]) -> Result<Option<SnapshotRef>, String> {
    let all = snapshots::list(&ctx.data_dir.join(STORE))
        .map_err(|e| format!("the store's snapshots can't be listed: {e}"))?;
    let Some(s) = all
        .into_iter()
        .filter(|s| !before.contains(&s.name))
        .filter_map(|s| match s.kind {
            SnapKind::PreMigration { version, repeat } => Some(((version, repeat), s)),
            SnapKind::BeforeRollback => None,
        })
        .min_by_key(|(k, _)| *k)
        .map(|(_, s)| s)
    else {
        return Ok(None);
    };
    let (sha, _) = hash(&s.path).map_err(|e| format!("{}: {e}", s.name))?;
    Ok(Some(SnapshotRef {
        name: s.name,
        sha256: sha.to_hex(),
    }))
}

/// Step 3: the snapshot over the store, beneath the pinned data folder.
fn restore(ctx: &Context, sys: &dyn System, s: &SnapshotRef) -> Result<(), String> {
    let _pin = layout::pin_dir(&ctx.data_dir).map_err(|e| format!("the data folder: {e}"))?;
    let dir = &ctx.data_dir;
    if s.name.contains(['/', '\\']) || snapshots::parse_name(STEM, &s.name).is_none() {
        return Err("the snapshot's name isn't one beside the store".into());
    }
    let snap = dir.join(&s.name);
    let store = dir.join(STORE);
    if std::fs::symlink_metadata(&snap).is_err() {
        // Gone: done only if the store is that snapshot.
        let (sha, _) = hash(&store).map_err(|e| format!("the store: {e}"))?;
        return if sha.to_hex() == s.sha256 {
            Ok(())
        } else {
            Err(format!(
                "the snapshot {} is gone and the store isn't it",
                s.name
            ))
        };
    }
    // A snapshot with its own journal or WAL beside it was mid-write:
    // SYSTEM never opens SQLite to settle it, so it isn't restored.
    for side in ["-wal", "-journal"] {
        if std::fs::symlink_metadata(dir.join(format!("{}{side}", s.name))).is_ok() {
            return Err(format!(
                "the snapshot {} has a {side} file beside it",
                s.name
            ));
        }
    }
    let (sha, _) = hash(&snap).map_err(|e| format!("{}: {e}", s.name))?;
    if sha.to_hex() != s.sha256 {
        return Err(format!(
            "the snapshot {} changed since it was chosen",
            s.name
        ));
    }
    for side in ["-wal", "-shm", "-journal"] {
        match std::fs::remove_file(dir.join(format!("{STORE}{side}"))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("removing the store's {side} file: {e}"));
            }
            _ => {}
        }
    }
    sys.copied("sides_removed");
    // Still a regular file of its own, right before the rename.
    if !std::fs::symlink_metadata(&snap).is_ok_and(|m| m.is_file()) {
        return Err(format!("the snapshot {} isn't a regular file", s.name));
    }
    layout::rename_retrying(&snap, &store).map_err(|e| format!("restoring the store: {e}"))
}

/// After a rollback, the reinstalled MSI is the current one again.
/// Idempotent: done once `current\` holds `from`'s pins.
fn rotate_back(ctx: &Context, from: &str) {
    let (current, previous) = (ctx.staging.current(), ctx.staging.previous());
    let current_is = layout::read_json::<Pins>(&current.join(PINS))
        .ok()
        .flatten()
        .is_some_and(|p| p.version == from);
    if current_is {
        let _ = helper::remove_dir(&previous);
        return;
    }
    let moved =
        helper::remove_dir(&current).and_then(|()| layout::rename_retrying(&previous, &current));
    if let Err(e) = moved {
        eprintln!("lodestar update-helper: keeping the reinstalled MSI as current: {e}");
    }
}

/// The SHA-256 and size of a file opened as itself (never through a link
/// the service account planted).
pub(super) fn hash(path: &Path) -> std::io::Result<(Sha256, u64)> {
    let mut f = layout::open_nofollow(path)?;
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
