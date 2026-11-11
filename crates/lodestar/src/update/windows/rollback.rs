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
//! **Across versions:** the journal types accept unknown fields, because
//! after a reinstall the older helper reads the journal the newer one
//! wrote. A new field must be safe for an older helper to ignore, with a
//! `#[serde(default)]` that keeps the old behaviour; a field an older
//! helper mustn't ignore bumps `FORMAT` instead, so that helper refuses the
//! journal. (`touched` and `reinstall_restart` aren't safe to ignore; they
//! stand only because no released helper has journal code without them.)
//!
//! - **Installing:** written before `msiexec` runs. Probation follows: the
//!   new service must reach Running and stay there
//!   ([`super::helper::System::healthy`]). Passed, the journal goes. The
//!   next run tells what happened from the helper's own version (the
//!   program on disk, a SYSTEM-owned fact): still `from` means the install
//!   never happened, or waits for a restart (exit 3010; for at most
//!   [`MAX_RESTARTS`] boots); `to` means judge
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
//!      folder. The marker stays until step 5. A repeat that finds the snapshot
//!      gone and the store at its SHA-256 counts it done;
//!   4. **then the previous MSI,** found by its pins (version `from`, size,
//!      SHA-256) in staging's `previous\` or `current\`, checked right
//!      before it runs (rule 4's one exception: the journal's rollback
//!      target with SYSTEM-held pins);
//!   5. after a reinstall that needs a restart, waiting (disabled and
//!      stopped on every pass) until the program on disk is `from`, for
//!      at most [`MAX_RESTARTS`] boots (then it halts); then
//!      the marker removed, the start type back to what it was before the install (recorded
//!      before its stop, since the MSI resets it to delayed automatic;
//!      delayed automatic when none was recorded), the service started, the
//!      reinstalled MSI made `current\` again; then the old version's
//!      health is checked and reported.
//!
//!   Steps 1, 3 and 4 stop the service and check for its processes on
//!   every pass, so a resume never finds it running.
//! - **Stops:** a transient one (the service won't stop yet, a process
//!   lingers, Windows Installer is busy) leaves the journal, the service
//!   disabled, for the next run to retry ([`Outcome::RollbackPending`]):
//!   until the next boot, unless someone starts the task. A
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
    /// The service's start type before the install stopped it, recorded
    /// then because the MSI resets it to delayed automatic: what a healthy
    /// probation or a finished rollback sets back. `None` when it was
    /// disabled or couldn't be read; then delayed automatic (the MSI's).
    #[serde(default)]
    pub start_before: Option<Start>,
    /// The rollback has touched the service (step 1 began): a halt from
    /// here on leaves it disabled and stopped. Before, nothing changed.
    #[serde(default)]
    pub touched: bool,
    /// The `msiexec` run handed to the helper's copy, while it's pending:
    /// what the copy runs, and nothing else.
    #[serde(default)]
    pub msi: Option<MsiRun>,
    /// The copy's answer to `msi`, recorded here before its file goes, so
    /// a crash between the two loses nothing.
    #[serde(default)]
    pub answer: Option<MsiResult>,
    /// Copies started for this rollback's reinstall (bounded by
    /// [`MAX_COPIES`]).
    #[serde(default)]
    pub copies: u32,
    /// The rollback's reinstall waits for Windows to restart (exit 3010 or
    /// 1641): step 5 waits until the program on disk is `from`.
    #[serde(default)]
    pub reinstall_restart: bool,
    /// While a 3010 waits for a restart (the install's, `from` still on
    /// disk; or the rollback's reinstall at step 5, `to` still on disk):
    /// the boot (Windows' boot time, Unix seconds) it last saw the old
    /// program in. A later boot that still sees it is a restart that didn't
    /// replace it ([`MAX_RESTARTS`]). An older helper ignores it and waits
    /// as before.
    #[serde(default)]
    pub restart_boot: Option<u64>,
    /// Restarts that didn't put `from` back.
    #[serde(default)]
    pub restarts: u32,
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
pub struct MsiRun {
    pub purpose: Purpose,
    /// The MSI, inside staging.
    pub msi: String,
    /// Its log, inside staging.
    pub log: String,
    /// What the MSI was checked against (checked again by the copy).
    pub pins: Pins,
    /// The copy's file name in staging (`run-<32 hex>.exe`), chosen before
    /// the journal is written: its answer must name it.
    #[serde(default)]
    pub copy: String,
}

/// Fresh copies a rollback's reinstall may be handed to before it halts.
pub const MAX_COPIES: u32 = 3;
/// Restarts after a reinstall's 3010 that still find `to` on disk before
/// the rollback halts (LOW-B): the pending replacement was cancelled or
/// failed. Two, so one boot misread (a clock change) doesn't halt it.
pub const MAX_RESTARTS: u32 = 2;
/// Two boot times further apart than this are two boots: the boot time is
/// read to the second, and moves only with a change of the clock.
pub const BOOT_SLACK: u64 = 60;
/// How long [`copy_running`] tries to remove a copy whose lock is free:
/// the copy starts the task again right before it ends, so the run that
/// follows can find its image still mapped for a moment (P-4).
pub const COPY_EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// In staging: the copy's answer, `msiexec`'s exit code.
pub const MSI_RESULT: &str = "msiexec-result.json";
/// In staging: held by the copy while it runs.
pub const MSI_LOCK: &str = "msiexec.lock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsiResult {
    pub id: String,
    pub purpose: Purpose,
    /// The copy that ran it.
    #[serde(default)]
    pub copy: String,
    pub code: i32,
    /// The copy refused before `msiexec` ran (`code` means nothing then).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// What the copy's answer says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Code(i32),
    /// It refused before `msiexec` ran: nothing changed.
    Refused(String),
}

/// The copy's answer for this journal's pending run, consumed: read and
/// removed. `None` when there's none (yet), or it isn't this run's (its
/// journal id, purpose and copy must all match).
pub fn take_result(ctx: &Context, j: &mut Journal) -> Option<Answer> {
    let run = j.msi.as_ref()?;
    let r = match j.answer.clone() {
        Some(r) => {
            // Recorded before a crash: its file may still be there.
            let _ = std::fs::remove_file(ctx.staging.dir.join(MSI_RESULT));
            r
        }
        None => {
            let path = ctx.staging.dir.join(MSI_RESULT);
            let r: MsiResult = layout::read_json(&path).ok().flatten()?;
            if r.id != j.id || r.purpose != run.purpose || r.copy != run.copy {
                let _ = std::fs::remove_file(&path);
                return None;
            }
            // Recorded in the journal first, then the file goes.
            j.answer = Some(r.clone());
            write(ctx, j).ok()?;
            let _ = std::fs::remove_file(&path);
            r
        }
    };
    Some(match r.refused {
        Some(why) => Answer::Refused(why),
        None => Answer::Code(r.code),
    })
}

pub fn write_result(ctx: &Context, r: &MsiResult) -> Result<(), Refusal> {
    let bytes = serde_json::to_vec(r).map_err(|e| Refusal::Io(e.to_string()))?;
    layout::write_atomic(&ctx.staging.dir, MSI_RESULT, &bytes)
        .map_err(|e| Refusal::Io(format!("{MSI_RESULT}: {e}")))
}

/// Takes the copy's lock in staging; `None` while a copy holds it. Only
/// the copy creates the file (`create`), once it has checked and pinned
/// staging; a check that finds no file finds no copy.
pub fn msi_lock(ctx: &Context, create: bool) -> std::io::Result<Option<std::fs::File>> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(ctx.staging.dir.join(MSI_LOCK))?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Whether a copy may be running: its lock is held, the lock can't be
/// read, or this run's copy is still there (a starting copy that hasn't
/// taken the lock yet can't be removed: its image is running). A copy
/// that can't be removed is tried again for up to [`COPY_EXIT_WAIT`]: one
/// that has just started this run is still ending.
pub fn copy_running(ctx: &Context, j: Option<&Journal>) -> bool {
    copy_running_with(
        ctx,
        j,
        &|p| std::fs::remove_file(p),
        COPY_EXIT_WAIT,
        std::time::Instant::now,
        std::thread::sleep,
    )
}

/// How often [`copy_running`] tries the removal again.
const COPY_EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// [`copy_running`], with the removal, the clock and the sleep passed in.
/// The lock is looked at once, first: looking again while the copy starts
/// could take it from under the copy.
pub(super) fn copy_running_with(
    ctx: &Context,
    j: Option<&Journal>,
    remove: &dyn Fn(&Path) -> std::io::Result<()>,
    wait: std::time::Duration,
    now: impl Fn() -> std::time::Instant,
    mut sleep: impl FnMut(std::time::Duration),
) -> bool {
    match msi_lock(ctx, false) {
        Ok(Some(_)) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => return true,
    }
    let Some(copy) = j.and_then(|j| j.msi.as_ref()).map(|r| r.copy.as_str()) else {
        return false;
    };
    if copy.is_empty() {
        return false;
    }
    let path = ctx.staging.dir.join(copy);
    let start = now();
    loop {
        if std::fs::symlink_metadata(&path).is_err() || remove(&path).is_ok() {
            return false;
        }
        if now().saturating_duration_since(start) >= wait {
            return true;
        }
        sleep(COPY_EXIT_POLL);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "step")]
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
pub struct SnapshotRef {
    /// A file name beside the store, never a path.
    pub name: String,
    pub sha256: String,
}

/// The journal in staging, if any. One that can't be read refuses the
/// run rather than guess.
pub fn read(ctx: &Context) -> Result<Option<Journal>, Refusal> {
    match layout::read_json::<Journal>(&ctx.staging.dir.join(JOURNAL)) {
        Ok(Some(j)) if j.format == FORMAT && valid(ctx, &j) => Ok(Some(j)),
        Ok(None) => Ok(None),
        _ => Err(Refusal::Io("journal.json can't be read".into())),
    }
}

/// A journal's path fields, checked on read: the copy is a `run-<32
/// hex>.exe`, and the MSI and its log are inside staging.
fn valid(ctx: &Context, j: &Journal) -> bool {
    let Some(run) = &j.msi else {
        return true;
    };
    valid_copy(&run.copy)
        && inside_staging(ctx, Path::new(&run.msi))
        && inside_staging(ctx, Path::new(&run.log))
}

/// `run-<32 lowercase hex>.exe`.
pub fn valid_copy(name: &str) -> bool {
    name.strip_prefix("run-")
        .and_then(|n| n.strip_suffix(".exe"))
        .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// Inside the staging folder, with no `..` step.
pub fn inside_staging(ctx: &Context, p: &Path) -> bool {
    p.starts_with(&ctx.staging.dir) && !p.components().any(|c| c == std::path::Component::ParentDir)
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
        answer: None,
        copies: 0,
        reinstall_restart: false,
        restart_boot: None,
        restarts: 0,
        touched: false,
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
        // The boot it came in bounds the wait.
        journal.restart = true;
        journal.restart_boot = sys.boot_time().ok();
        let _ = write(ctx, &journal);
        return Outcome::Installed {
            version: to,
            restart: true,
        };
    }
    if sys.healthy() {
        // The MSI installs it delayed automatic: what the administrator had
        // before goes back.
        if let Some(start) = journal.start_before.filter(|s| *s != Start::Auto) {
            let _ = sys.set_start(start);
        }
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
        let _ = remove_marker(ctx, sys);
        return Ok(None);
    }
    // An install handed to the copy: its answer first, or it's still
    // running. Without either the copy died; the program on disk decides.
    let mut journal = journal;
    if journal.state == State::Installing && journal.msi.is_some() {
        match take_result(ctx, &mut journal) {
            Some(Answer::Code(code)) => {
                return Ok(Some(helper::after_install(ctx, sys, journal, code)));
            }
            Some(Answer::Refused(why)) => {
                // Nothing ran: nothing to roll back, nothing skipped.
                clear(ctx);
                let _ = sys.set_start(journal.start_before.unwrap_or(Start::Auto));
                sys.start_service();
                sys.event_log(&format!(
                    "Lodestar's update helper didn't install {}: its copy refused ({why}). \
                     Nothing was changed.",
                    journal.to
                ));
                return Ok(Some(Outcome::CopyRefused { why }));
            }
            None if copy_running(ctx, Some(&journal)) => return Ok(Some(Outcome::HandedOff)),
            None => {}
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
            // Windows hasn't restarted yet: nothing to judge. Bounded: after
            // [`MAX_RESTARTS`] restarts that left `from` in place the
            // replacement never took effect, so the install didn't happen.
            if boot_seen(sys, &mut journal) {
                if journal.restarts >= MAX_RESTARTS {
                    clear(ctx);
                    let _ = sys.set_start(journal.start_before.unwrap_or(Start::Auto));
                    sys.start_service();
                    sys.event_log(&format!(
                        "Lodestar's update helper: Windows restarted {} times and {} still \
                         isn't in place, so its install didn't take effect. {} keeps running.",
                        journal.restarts, journal.to, journal.from
                    ));
                    return Ok(Some(Outcome::InstallInterrupted {
                        version: journal.to.clone(),
                    }));
                }
                let _ = write(ctx, &journal);
            }
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
            // A crash or restart may have left it stopped, or disabled by a
            // rollback whose `touched` save failed (the journal still says
            // installing).
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
/// (it goes only at step 5). Before step 1 nothing was changed. The
/// journal, the Event Log and `result.json` say how to recover.
fn halt(ctx: &Context, sys: &dyn System, mut j: Journal, done: u8, why: String) -> Outcome {
    // Recorded before the rollback first touches the service: without it
    // nothing was changed.
    // A journal without the field (an earlier build) counts any rollback
    // state as touched, as before the field existed.
    let touched = j.touched || matches!(j.state, State::RollingBack { .. });
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
        "The store may be half restored."
    } else if j.reinstall_restart {
        "The store was restored and the previous version reinstalled, but Windows didn't put \
         it in place at a restart: the replacement waiting for a restart didn't take effect."
    } else {
        "The store was restored, but reinstalling the previous version didn't finish."
    };
    let store = format!(
        "{store} {} stays until the rollback finishes, so every store opener refuses while it \
         exists.",
        marker.display()
    );
    format!(
        "Lodestar {to} failed its health check, and the rollback to {from} stopped after step \
         {done} of 5: {why}. The Lodestar service is stopped and disabled, so neither version \
         opens the store. {store} To recover, an administrator: 1. reads {journal} and the \
         msiexec logs beside it; 2. installs the {from} MSI by hand (the service it starts stays idle \
         while the marker exists, so the install completes), or restores a backup of \
         {data}; 3. once the store is one the installed version reads, deletes {marker} and \
         {journal}, then runs: sc config Lodestar start= delayed-auto, and: sc start Lodestar. \
         Installing a version other than {from} or {to} clears both files at the next \
         restart (the helper's run at boot). Neither \
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
    // The start type to go back to was recorded by the install, before
    // its stop (`start_before`); none recorded means delayed automatic.
    if done < 1 {
        if !j.touched {
            // Saved first, so a crash from here on resumes the rollback.
            j.touched = true;
            save(j, &snapshot, done)?;
        }
        stopped(sys)?;
        let bin = halt_on(
            Bin::existing(&ctx.data_dir)
                .map_err(|e| format!("bin: {e}"))
                .and_then(|b| b.ok_or_else(|| "bin is missing".to_string())),
        )?;
        let lock =
            helper::take_lock(sys, &bin, ctx.lock_wait).map_err(|e| Stop::Retry(e.to_string()))?;
        halt_on(
            bin.write(MARKER, &serde_json::json!({ "windows_rollback": j.to }))
                .map_err(|e| format!("the rollback marker: {e}")),
        )?;
        // Best effort, as `mark_failed`: an unreadable `skipped` mustn't
        // stop the rollback (the next check offers the version again).
        let _ = super::check::add_skip(&bin, &j.to, super::check::SKIP_FAILED);
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
        // The marker stays until step 5: whatever starts the service before
        // then (the MSI's own StartServices, a failed version still on disk
        // after a 3010) refuses before it opens the restored store.
        done = 3;
        save(j, &snapshot, done)?;
    }
    if done < 4 {
        let code = match take_result(ctx, j) {
            Some(Answer::Code(code)) => code,
            Some(Answer::Refused(why)) => {
                j.msi = None;
                j.answer = None;
                j.copies = 0;
                save(j, &snapshot, done)?;
                return Err(Stop::Retry(format!("the helper's copy refused: {why}")));
            }
            None if copy_running(ctx, Some(j)) => return Err(Stop::HandedOff),
            None if j.copies >= MAX_COPIES => {
                return Err(Stop::Halt(format!(
                    "the reinstall of {} was handed to {MAX_COPIES} copies of the helper and \
                     none answered",
                    j.from
                )));
            }
            None => {
                stopped(sys)?;
                // Checked again right before it runs.
                let (staged, pins) = halt_on(previous_msi(ctx, &j.from))?;
                let log = ctx.staging.dir.join(ROLLBACK_LOG);
                let copy = helper::copy_name().map_err(|e| Stop::Retry(e.to_string()))?;
                j.msi = Some(MsiRun {
                    purpose: Purpose::Rollback,
                    msi: staged.to_string_lossy().into_owned(),
                    log: log.to_string_lossy().into_owned(),
                    pins,
                    copy: copy.clone(),
                });
                j.copies += 1;
                save(j, &snapshot, done)?;
                let args = msi::msiexec_args(&staged, &log);
                match sys.run_msiexec(&ctx.staging.dir, &args, &copy) {
                    Ok(helper::Ran::Code(c)) => c,
                    Ok(helper::Ran::HandedOff) => return Err(Stop::HandedOff),
                    // Never from Program Files instead: retried.
                    Err(why) => return Err(Stop::Retry(format!("the helper's copy: {why}"))),
                }
            }
        };
        j.msi = None;
        j.answer = None;
        // A copy answered: the bound counts copies in a row that didn't.
        j.copies = 0;
        match msi::installed(code) {
            msi::Installed::Done => {}
            msi::Installed::RestartNeeded => {
                // The old program may replace the failed one only at the
                // next restart: until then step 5 keeps the service disabled
                // and stopped (checked, so a failure is retried), and the
                // marker keeps the failed one off the restored store.
                j.reinstall_restart = true;
            }
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
        // After a reinstall that waits for a restart, step 5 waits for the
        // program on disk to be `from` (the boot run's).
        if j.reinstall_restart
            && ctx.running.as_ref().map(|v| v.to_string()) != Some(j.from.clone())
        {
            // On every pass: whatever started it meanwhile is stopped again.
            stopped(sys)?;
            restarted(ctx, sys, j, &snapshot, done)?;
            return Err(Stop::Retry(format!(
                "waiting for Windows to restart, which puts {} back in place",
                j.from
            )));
        }
        if !remove_marker(ctx, sys) {
            return Err(Stop::Retry(
                "the rollback marker couldn't be removed".into(),
            ));
        }
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

/// A 3010's wait for a restart, bounded (LOW-B): a boot other than the
/// one it last saw the old program in, with that program still on disk,
/// is a restart that didn't replace it, counted in `restarts`. Whether the
/// journal changed (it's the caller's to save). A boot time that can't be
/// read counts nothing (the wait goes on, as before the bound).
fn boot_seen(sys: &dyn System, j: &mut Journal) -> bool {
    let Ok(boot) = sys.boot_time() else {
        return false;
    };
    match j.restart_boot {
        Some(seen) if boot.abs_diff(seen) <= BOOT_SLACK => return false,
        Some(_) => j.restarts += 1,
        None => {}
    }
    j.restart_boot = Some(boot);
    true
}

/// Step 5's wait for a restart: [`MAX_RESTARTS`] boots that still find
/// `to` on disk halt the rollback.
fn restarted(
    ctx: &Context,
    sys: &dyn System,
    j: &mut Journal,
    snapshot: &Option<SnapshotRef>,
    done: u8,
) -> Result<(), Stop> {
    if !boot_seen(sys, j) {
        return Ok(());
    }
    if j.restarts >= MAX_RESTARTS {
        return Err(Stop::Halt(format!(
            "Windows restarted {} times since the reinstall of {} asked for a restart, and {} \
             is still the program on disk: the replacement didn't take effect",
            j.restarts, j.from, j.to
        )));
    }
    j.state = State::RollingBack {
        snapshot: snapshot.clone(),
        done,
    };
    write(ctx, j).map_err(|e| Stop::Halt(e.to_string()))
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

/// Under `.lock`: the marker goes at step 5, right before the service is
/// started (or when the journal is dropped). Whether it's gone. The lock is
/// waited for, up to `lock_wait` ([`helper::take_lock`]): the service that
/// just stopped may not have let go of it yet. No marker, no lock: a
/// version installed by hand may be serving, holding it for good.
fn remove_marker(ctx: &Context, sys: &dyn System) -> bool {
    let marker = ctx.data_dir.join("bin").join(MARKER);
    if std::fs::symlink_metadata(&marker).is_err() {
        return true;
    }
    if let Ok(Some(bin)) = Bin::existing(&ctx.data_dir)
        && let Ok(_lock) = helper::take_lock(sys, &bin, ctx.lock_wait)
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
