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
    /// The rollback's reinstalls in a row that failed on 1920
    /// ([`msi::held_open`]), bounded by [`msi::MAX_BLOCKED`].
    #[serde(default)]
    pub blocked: u32,
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
        blocked: 0,
        touched: false,
        state: State::Installing,
    })
}

/// In staging: what the last install that passed probation leaves for a
/// manual Roll back ([`Installed`]).
pub const INSTALLED: &str = "installed.json";
/// The `skipped` reason a manual Roll back gives the version it leaves:
/// like `failed`, one the user can't undo (`check::refusing`).
pub const SKIP_MANUAL: &str = "manual";
/// A manual Roll back's journal id starts with this, so its wording and
/// skip reason are a manual one's. An older helper that resumes it only
/// words it as a failed probation's.
pub const MANUAL_ID: &str = "manual-";

/// What a manual Roll back needs from the install that passed probation:
/// its versions, its start type before, and the snapshot it took
/// (by name). Kept in staging, which only SYSTEM and Administrators can
/// write, so nothing in the data folder decides a rollback. Replaced by
/// the next install that passes, removed by a finished rollback; one whose
/// `to` isn't the running version is never used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub format: u32,
    pub from: String,
    pub to: String,
    /// The earliest pre-migration snapshot `to` took ([`choose`]'s rule),
    /// named while it was certainly there; `None` when it took none (it
    /// didn't migrate the store, so a rollback keeps it).
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub start_before: Option<Start>,
}

/// Why a manual Roll back can't run, as the API's `rollback_unavailable`
/// reasons name it ([`ManualRefusal::code`]).
// Not wired to a request yet (the helper's request and the API are
// X-win-upd-manual-rollback's follow-up): only tests call it.
#[cfg_attr(all(windows, not(test)), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualRefusal {
    /// A journal is in place: an install, its probation or a rollback.
    Busy,
    /// No install that passed probation is recorded for the running
    /// version, or its previous MSI isn't kept (or doesn't match its pins).
    NoPreviousVersion(String),
    /// The snapshot the install took is gone: restoring nothing would
    /// leave the previous version a store it can't read.
    SnapshotMissing(String),
    /// Staging or the store's folder couldn't be read or written.
    Io(String),
}

impl ManualRefusal {
    #[cfg_attr(all(windows, not(test)), allow(dead_code))]
    pub fn code(&self) -> &'static str {
        match self {
            ManualRefusal::Busy => "busy",
            ManualRefusal::NoPreviousVersion(_) => "no_previous_version",
            ManualRefusal::SnapshotMissing(_) => "snapshot_missing",
            ManualRefusal::Io(_) => "io",
        }
    }
}

/// What a manual Roll back would do, read without writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualPlan {
    /// The version reinstalled.
    pub from: String,
    /// The running version, rolled back.
    pub to: String,
    /// The snapshot restored over the store (by name), or `None` when the
    /// store is kept.
    pub snapshot: Option<String>,
}

/// Whether the journal is a manual Roll back's.
pub fn is_manual(j: &Journal) -> bool {
    j.id.starts_with(MANUAL_ID)
}

/// Best effort, on a passed probation: a failure only means no manual
/// Roll back is offered for this install, never that it's wrong.
fn keep_installed(ctx: &Context, j: &Journal) {
    let snapshot = match choose_name(ctx, &j.snapshots_before) {
        Ok(s) => s,
        Err(e) => {
            // Without knowing whether it migrated the store, no manual
            // Roll back: a stale record mustn't stay either.
            eprintln!(
                "lodestar update-helper: no manual rollback for {}: {e}",
                j.to
            );
            let _ = std::fs::remove_file(ctx.staging.dir.join(INSTALLED));
            return;
        }
    };
    let rec = Installed {
        format: FORMAT,
        from: j.from.clone(),
        to: j.to.clone(),
        snapshot,
        start_before: j.start_before,
    };
    let written = serde_json::to_vec_pretty(&rec)
        .map_err(|e| e.to_string())
        .and_then(|b| {
            layout::write_atomic(&ctx.staging.dir, INSTALLED, &b).map_err(|e| e.to_string())
        });
    if let Err(e) = written {
        eprintln!("lodestar update-helper: {INSTALLED}: {e}");
        let _ = std::fs::remove_file(ctx.staging.dir.join(INSTALLED));
    }
}

/// What a manual Roll back of `running` would do, or why it can't run.
/// Reads staging and lists the store's folder; writes nothing. Fails
/// closed: a record it can't read, a version that doesn't parse or isn't
/// older, a missing or mismatched MSI and a missing snapshot all refuse.
// Not wired yet: the service's availability check (the follow-up slice)
// calls it; only tests do now.
#[cfg_attr(all(windows, not(test)), allow(dead_code))]
pub fn manual_plan(ctx: &Context, running: &Version) -> Result<ManualPlan, ManualRefusal> {
    plan_with_record(ctx, running).map(|(plan, _)| plan)
}

/// [`manual_plan`], with the record it came from.
fn plan_with_record(
    ctx: &Context,
    running: &Version,
) -> Result<(ManualPlan, Installed), ManualRefusal> {
    if read(ctx)
        .map_err(|e| ManualRefusal::Io(e.to_string()))?
        .is_some()
    {
        return Err(ManualRefusal::Busy);
    }
    let rec = installed(ctx, running)?;
    previous_msi(ctx, &rec.from).map_err(ManualRefusal::NoPreviousVersion)?;
    if let Some(name) = &rec.snapshot {
        let all = snapshots::list(&ctx.data_dir.join(STORE))
            .map_err(|e| ManualRefusal::Io(format!("the store's snapshots: {e}")))?;
        if !all.iter().any(|s| &s.name == name) {
            return Err(ManualRefusal::SnapshotMissing(format!(
                "the snapshot {name} that {} took is gone",
                rec.to
            )));
        }
    }
    let plan = ManualPlan {
        from: rec.from.clone(),
        to: rec.to.clone(),
        snapshot: rec.snapshot.clone(),
    };
    Ok((plan, rec))
}

/// Run once the service has stopped, so nothing deletes a snapshot after
/// it: the snapshot a manual Roll back restores, still as the record names
/// it (the service's retention purge may have deleted it since the plan).
fn manual_snapshot(ctx: &Context, j: &Journal) -> Result<Option<String>, String> {
    let rec: Installed = layout::read_json(&ctx.staging.dir.join(INSTALLED))
        .map_err(|e| format!("{INSTALLED}: {e}"))?
        .ok_or_else(|| format!("{INSTALLED} is gone"))?;
    if rec.from != j.from || rec.to != j.to {
        return Err(format!("{INSTALLED} is for another update"));
    }
    if let Some(name) = &rec.snapshot {
        let all = snapshots::list(&ctx.data_dir.join(STORE))
            .map_err(|e| format!("the store's snapshots: {e}"))?;
        if !all.iter().any(|s| &s.name == name) {
            return Err(format!("the snapshot {name} that {} took is gone", rec.to));
        }
    }
    Ok(rec.snapshot)
}

/// The record for `running`, checked.
fn installed(ctx: &Context, running: &Version) -> Result<Installed, ManualRefusal> {
    let none = |why: &str| ManualRefusal::NoPreviousVersion(why.to_string());
    let rec: Installed = match layout::read_json(&ctx.staging.dir.join(INSTALLED)) {
        Ok(Some(r)) => r,
        Ok(None) => return Err(none("no update installed by the helper is recorded")),
        Err(e) => return Err(ManualRefusal::Io(format!("{INSTALLED}: {e}"))),
    };
    if rec.format != FORMAT {
        return Err(ManualRefusal::Io(format!(
            "{INSTALLED} has an unknown format"
        )));
    }
    let (Some(from), Some(to)) = (
        layout::parse_version(&rec.from),
        layout::parse_version(&rec.to),
    ) else {
        return Err(ManualRefusal::Io(format!(
            "{INSTALLED}'s versions are invalid"
        )));
    };
    if &to != running {
        return Err(none("the running version wasn't installed by the helper"));
    }
    if from >= to {
        return Err(none("the recorded previous version isn't older"));
    }
    // Tied to what the helper itself keeps: the install leaves `to`'s MSI
    // as `current\`. A record a hand install or an older helper left
    // stale doesn't match it.
    let current = layout::read_json::<Pins>(&ctx.staging.current().join(PINS))
        .ok()
        .flatten();
    if current.is_none_or(|p| p.version != rec.to) {
        return Err(none("the helper didn't install the running version"));
    }
    // Names beside the store, nothing else: `restore` checks again.
    if let Some(n) = &rec.snapshot
        && (n.contains(['/', '\\'])
            || !matches!(
                snapshots::parse_name(STEM, n),
                Some(SnapKind::PreMigration { .. })
            ))
    {
        return Err(ManualRefusal::Io(format!(
            "{INSTALLED}'s snapshot isn't one beside the store"
        )));
    }
    Ok(rec)
}

/// The journal a manual Roll back of `running` begins with, from
/// [`manual_plan`]: `from` the recorded previous version, `to` the
/// running one. Its `snapshots_before` is every snapshot there now but the
/// recorded one, so step 2's [`choose`] finds exactly that one (or none,
/// when the store is kept). `request` names the request it came from: 1 to
/// 32 lowercase hex digits.
pub fn manual_journal(
    ctx: &Context,
    running: &Version,
    request: &str,
) -> Result<Journal, ManualRefusal> {
    if request.is_empty()
        || request.len() > 32
        || !request
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(ManualRefusal::Io("the request's id is invalid".into()));
    }
    let (plan, rec) = plan_with_record(ctx, running)?;
    let all = snapshots::list(&ctx.data_dir.join(STORE))
        .map_err(|e| ManualRefusal::Io(format!("the store's snapshots: {e}")))?;
    let before = all
        .into_iter()
        .map(|s| s.name)
        .filter(|n| Some(n) != plan.snapshot.as_ref())
        .collect();
    Ok(Journal {
        format: FORMAT,
        id: format!("{MANUAL_ID}{request}"),
        from: plan.from,
        to: plan.to,
        snapshots_before: before,
        restart: false,
        start_before: rec.start_before,
        msi: None,
        answer: None,
        copies: 0,
        reinstall_restart: false,
        restart_boot: None,
        restarts: 0,
        blocked: 0,
        touched: false,
        state: State::RollingBack {
            snapshot: None,
            done: 0,
        },
    })
}

/// A manual Roll back of `running` (architecture §12.2: "Roll back in
/// Settings → Updates, the previous version, while its snapshot is
/// kept"), run by the helper for a request, after [`resume`]. The journal
/// is written first, then the same five steps a failed probation's
/// rollback takes, so a crash or restart is finished the same way. The
/// start type it puts back is the one now (`start_before` when it can't
/// be read or is disabled).
// Not wired to a request yet (the helper's request and the API are
// X-win-upd-manual-rollback's follow-up): only tests call it.
#[cfg_attr(all(windows, not(test)), allow(dead_code))]
pub fn manual(
    ctx: &Context,
    sys: &dyn System,
    running: &Version,
    request: &str,
) -> Result<Outcome, ManualRefusal> {
    let mut j = manual_journal(ctx, running, request)?;
    if let Ok(start) = sys.start_type()
        && start != Start::Disabled
    {
        j.start_before = Some(start);
    }
    write(ctx, &j).map_err(|e| ManualRefusal::Io(e.to_string()))?;
    Ok(roll_back(ctx, sys, j))
}

/// The opening of a halt's text: what was rolled back, and why.
fn opening(j: &Journal, to: &str, from: &str) -> String {
    if is_manual(j) {
        format!("Lodestar's manual rollback from {to} to {from}")
    } else {
        format!("Lodestar {to} failed its health check, and the rollback to {from}")
    }
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
            Cause::Step,
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
        // What a manual Roll back needs later, before the journal goes.
        keep_installed(ctx, &journal);
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
                    "Lodestar's update helper didn't install {}: its copy refused ({}). \
                     Nothing was changed.",
                    shown_version(&journal.to, "the new version"),
                    shown_code(&why),
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
                        journal.restarts,
                        shown_version(&journal.to, "the new version"),
                        shown_version(&journal.from, "The installed version"),
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
    /// Final too: the journal couldn't be saved, so the rollback stops
    /// where it is (its recovery text says so, not what the step was).
    Journal(String),
    /// The reinstall was handed to the helper's copy; the run that
    /// follows it finishes.
    HandedOff,
    /// A manual Roll back found, with the service stopped, that it can't
    /// go on: before the marker or the skip, so the service is started
    /// again and nothing was changed.
    Undo(String),
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
            // `previous\` no longer holds the record's `from`: nothing is
            // left to roll back by hand.
            let _ = std::fs::remove_file(ctx.staging.dir.join(INSTALLED));
            Outcome::RolledBack {
                failed: j.to.clone(),
                to: j.from.clone(),
                healthy,
            }
        }
        Err(Stop::Retry(why)) => Outcome::RollbackPending { why },
        Err(Stop::HandedOff) => Outcome::HandedOff,
        Err(Stop::Undo(why)) => {
            let _ = sys.set_start(j.start_before.unwrap_or(Start::Auto));
            sys.start_service();
            j.touched = false;
            halt(ctx, sys, j, 0, why, Cause::Step)
        }
        Err(Stop::Halt(why)) => {
            let done = done_of(&j);
            halt(ctx, sys, j, done, why, Cause::Step)
        }
        Err(Stop::Journal(why)) => {
            let done = done_of(&j);
            halt(ctx, sys, j, done, why, Cause::Journal)
        }
    }
}

fn done_of(j: &Journal) -> u8 {
    match &j.state {
        State::RollingBack { done, .. } => *done,
        _ => 0,
    }
}

/// What made a rollback halt, for its recovery text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A step couldn't be done (or a bound ran out).
    Step,
    /// The journal couldn't be saved after a step.
    Journal,
}

/// A final stop. Neither version starts once the service was stopped
/// (§12.2): it stays stopped and disabled, with the marker in place
/// (it goes only at step 5). Before step 1 nothing was changed. The
/// journal, the Event Log and `result.json` say how to recover.
fn halt(
    ctx: &Context,
    sys: &dyn System,
    mut j: Journal,
    done: u8,
    why: String,
    cause: Cause,
) -> Outcome {
    // Recorded before the rollback first touches the service: without it
    // nothing was changed.
    // A journal without the field (an earlier build) counts any rollback
    // state as touched, as before the field existed.
    // A manual one starts rolling back at step 0, and always has the field.
    let touched = j.touched || (!is_manual(&j) && matches!(j.state, State::RollingBack { .. }));
    let step = if touched { done.max(1) } else { 0 };
    let recovery = recovery(ctx, &j, step, &why, cause);
    // The Event Log gets the same steps with fixed text: every local user
    // can read it, so no reason text and no folder path (a configured data
    // folder can sit in a user profile). The full text, paths and reason
    // included, is in the journal and `result.json`.
    sys.event_log(&recovery_event(&j, step, cause));
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

/// What the store holds after a halt at `done`.
fn store_state(j: &Journal, done: u8, cause: Cause) -> &'static str {
    if cause == Cause::Journal {
        if done < 3 {
            "The rollback's journal couldn't be saved, so it stopped there; the store may be \
             half restored."
        } else {
            "The rollback's journal couldn't be saved, so it stopped there; the store was \
             restored before it did."
        }
    } else if done < 3 {
        "The store may be half restored."
    } else if j.reinstall_restart {
        "The store was restored and the previous version reinstalled, but Windows didn't put \
         it in place at a restart: the replacement waiting for a restart didn't take effect."
    } else {
        "The store was restored, but reinstalling the previous version didn't finish."
    }
}

/// A version from the journal's own text, for the Event Log: shown only
/// as a version (semver holds `0-9 A-Z a-z . - +` alone), else as
/// `otherwise` (X-event-log-followups). A pre-release or build part is a
/// word, so the bound is the grammar, which rules out paths, spaces and
/// accounts (`DOMAIN\user`). The journal is in the private staging folder,
/// written by the helper from a signed manifest's versions.
pub(super) fn shown_version(s: &str, otherwise: &str) -> String {
    Version::parse(s).map_or_else(|_| otherwise.to_owned(), |v| v.to_string())
}

/// The copy's refusal, read back from its result file: a code (`a-z` and
/// `_`, as `msi::Refusal::code` writes) or fixed words. The shape is the
/// bound, not a list of codes: the file is in the private staging folder,
/// which only SYSTEM and Administrators can write, and the copy writes
/// only `Refusal::code()`. So a lowercase word that isn't a code could
/// come only from an administrator.
pub(super) fn shown_code(s: &str) -> &str {
    let code =
        !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    if code { s } else { "an unexpected reason" }
}

/// [`recovery`]'s steps for the Event Log, which every local user can
/// read: fixed text and parsed versions alone, the folders by role and the
/// files by name, and no reason text. The reason and the full paths are
/// in the journal and `result.json` ([`recovery`]).
pub fn recovery_event(j: &Journal, done: u8, cause: Cause) -> String {
    let from = shown_version(&j.from, "the previous version");
    let to = shown_version(&j.to, "the new version");
    let journal = format!("{JOURNAL} in Lodestar's update staging folder");
    if done == 0 {
        // Nothing was touched, so the journal is cleared (`halt`): the
        // reason is in the answer the helper leaves, `updates\result.json`.
        return format!(
            "{opening} couldn't start. Nothing was changed: {to} keeps running. The reason is in {updates}\\{result} in \
             Lodestar's data folder. To go back to {from}, an administrator installs the {from} \
             MSI by hand.",
            updates = msi::UPDATES,
            result = msi::RESULT,
            opening = opening(j, &to, &from),
        );
    }
    let marker = format!("bin\\{MARKER} in Lodestar's data folder");
    format!(
        "{opening} stopped after step {done} of 5. The Lodestar service is stopped and disabled, so neither version opens \
         the store. {store} {marker} stays until the rollback finishes, so every store opener \
         refuses while it exists. To recover, an administrator: 1. reads {journal} (it has the \
         reason) and the msiexec logs beside it; 2. installs the {from} MSI by hand (the service \
         it starts stays idle while the marker exists, so the install completes), or restores \
         a backup of Lodestar's data folder; 3. once the store is one the installed version \
         reads, deletes {marker} and {journal}, then runs: sc config Lodestar start= \
         delayed-auto, and: sc start Lodestar. Installing a version other than {from} or {to} \
         clears both files at the next restart (the helper's run at boot). Neither file starts \
         anything.",
        store = store_state(j, done, cause),
        opening = opening(j, &to, &from),
    )
}

/// What an administrator does after a halt: which step stopped, and how
/// to bring Lodestar back. Neither the marker nor the journal starts
/// anything by itself; removing them is the administrator's step, once
/// the store is one the installed version reads.
pub fn recovery(ctx: &Context, j: &Journal, done: u8, why: &str, cause: Cause) -> String {
    let (from, to) = (&j.from, &j.to);
    let journal = ctx.staging.dir.join(JOURNAL);
    if done == 0 {
        return format!(
            "{opening} couldn't start: {why}. Nothing was changed: {to} keeps running. To go \
             back to {from}, an administrator installs the {from} MSI by hand.",
            opening = opening(j, to, from),
        );
    }
    let marker = ctx.data_dir.join("bin").join(MARKER);
    let store = store_state(j, done, cause);
    let store = format!(
        "{store} {} stays until the rollback finishes, so every store opener refuses while it \
         exists.",
        marker.display()
    );
    format!(
        "{opening} stopped after step {done} of 5: {why}. The Lodestar service is stopped and disabled, so neither version \
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
        opening = opening(j, to, from),
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
        write(ctx, j).map_err(|e| Stop::Journal(e.to_string()))
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
        if is_manual(j) {
            // Checked again with the service stopped: its retention purge
            // can't delete the snapshot after this.
            manual_snapshot(ctx, j).map_err(Stop::Undo)?;
        }
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
        let reason = if is_manual(j) {
            SKIP_MANUAL
        } else {
            super::check::SKIP_FAILED
        };
        let _ = super::check::add_skip(&bin, &j.to, reason);
        drop(lock);
        done = 1;
        save(j, &snapshot, done)?;
    }
    if done < 2 {
        snapshot = halt_on(choose(ctx, &j.snapshots_before))?;
        if is_manual(j) {
            // Exactly the recorded one, or the store would be kept (or the
            // wrong one restored) under a version that can't read it.
            let want = halt_on(manual_snapshot(ctx, j))?;
            if snapshot.as_ref().map(|s| &s.name) != want.as_ref() {
                return Err(Stop::Halt(
                    "the snapshot found isn't the one the update took".into(),
                ));
            }
        }
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
                // The run's log is its own: an earlier one's 1920 is never read.
                let _ = std::fs::remove_file(&log);
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
        // Any other answer ends the reinstall's step (done, a restart, or a
        // halt), so the count needs no reset.
        let blocked = helper::blocked_by_1920(ctx, code, &ctx.staging.dir.join(ROLLBACK_LOG));
        match msi::installed(code) {
            msi::Installed::Failed(c)
                if blocked && j.blocked.saturating_add(1) < msi::MAX_BLOCKED =>
            {
                // The service couldn't start at the reinstall's end (1920),
                // as when lodestar.exe is held open: Windows Installer rolled
                // it back. Kept disabled and stopped, tried again next run.
                j.blocked = j.blocked.saturating_add(1);
                save(j, &snapshot, done)?;
                stopped(sys)?;
                return Err(Stop::Retry(format!(
                    "reinstalling {} failed ({c}): Windows Installer couldn't start the service \
                     (error 1920; another program may hold lodestar.exe open). Tried again at \
                     the next run ({} of {})",
                    j.from,
                    j.blocked,
                    msi::MAX_BLOCKED
                )));
            }
            msi::Installed::Failed(c) if blocked => {
                return Err(Stop::Halt(format!(
                    "reinstalling {} failed ({c}) {} times in a row: Windows Installer couldn't \
                     start the service (error 1920; another program may hold lodestar.exe open)",
                    j.from,
                    msi::MAX_BLOCKED
                )));
            }
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
    write(ctx, j).map_err(|e| Stop::Journal(e.to_string()))
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
    let Some(s) = earliest_new(ctx, before)? else {
        return Ok(None);
    };
    let (sha, _) = hash(&s.path).map_err(|e| format!("{}: {e}", s.name))?;
    Ok(Some(SnapshotRef {
        name: s.name,
        sha256: sha.to_hex(),
    }))
}

/// [`choose`]'s snapshot, by name only (nothing hashed).
fn choose_name(ctx: &Context, before: &[String]) -> Result<Option<String>, String> {
    Ok(earliest_new(ctx, before)?.map(|s| s.name))
}

/// The earliest pre-migration snapshot not in `before`.
fn earliest_new(ctx: &Context, before: &[String]) -> Result<Option<snapshots::Snapshot>, String> {
    let all = snapshots::list(&ctx.data_dir.join(STORE))
        .map_err(|e| format!("the store's snapshots can't be listed: {e}"))?;
    Ok(all
        .into_iter()
        .filter(|s| !before.contains(&s.name))
        .filter_map(|s| match s.kind {
            SnapKind::PreMigration { version, repeat } => Some(((version, repeat), s)),
            SnapKind::BeforeRollback => None,
        })
        .min_by_key(|(k, _)| *k)
        .map(|(_, s)| s))
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io;
    use std::sync::Mutex;
    use std::time::Duration;

    use lodestar_update::Channel;
    use lodestar_update::doc::Target;
    use lodestar_update::testing::TestTrust;

    use super::*;
    use crate::update::windows::msi::Staging;

    const OLD_MSI: &[u8] = b"the 0.6.0 msi";
    const SNAP: &str = "lodestar.pre-v8.db";

    /// A Windows that does what it's asked: every call is recorded.
    struct Fake {
        start: Start,
        unhealthy: bool,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(start: Start) -> Fake {
            Fake {
                start,
                unhealthy: false,
                calls: Mutex::new(Vec::new()),
            }
        }
        fn note(&self, s: impl Into<String>) {
            self.calls.lock().unwrap().push(s.into());
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl System for Fake {
        fn check_data(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn secure_staging(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn authenticode(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn stop_service(&self) -> io::Result<()> {
            self.note("stop");
            Ok(())
        }
        fn start_service(&self) {
            self.note("start");
        }
        fn msiexec(&self, _: &[OsString]) -> io::Result<i32> {
            unreachable!("msiexec runs through run_msiexec")
        }
        fn run_msiexec(&self, _: &Path, args: &[OsString], _: &str) -> Result<helper::Ran, String> {
            let msi = args
                .iter()
                .any(|a| a.to_string_lossy().ends_with("lodestar-0.6.0.msi"));
            self.note(format!("msiexec 0.6.0={msi}"));
            Ok(helper::Ran::Code(0))
        }
        fn healthy(&self) -> bool {
            !self.unhealthy
        }
        fn set_start(&self, start: Start) -> io::Result<()> {
            self.note(format!("set_start {start:?}"));
            Ok(())
        }
        fn start_type(&self) -> io::Result<Start> {
            Ok(self.start)
        }
        fn event_log(&self, message: &str) {
            self.note(format!("event {message}"));
        }
        fn service_processes(&self) -> io::Result<usize> {
            Ok(0)
        }
        fn boot_time(&self) -> io::Result<u64> {
            Ok(0)
        }
        fn installer_idle(&self, _: Duration) -> bool {
            true
        }
    }

    struct Env {
        _tmp: tempfile::TempDir,
        ctx: Context,
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    /// 0.7.0 running, installed by the helper from 0.6.0, which took
    /// `SNAP` before migrating; 0.6.0's MSI kept in `previous\`.
    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let trust = TestTrust::default();
        let ctx = Context {
            data_dir: tmp.path().join("data"),
            staging: Staging::of(&tmp.path().join("Program Files").join("Lodestar")),
            running: Some(v("0.7.0")),
            target: Some(Target::X86_64WindowsMsvc),
            now: 1_800_000_000,
            embedded: trust.embedded(vec![]),
            lock_wait: Duration::from_millis(300),
        };
        std::fs::create_dir_all(&ctx.data_dir).unwrap();
        std::fs::create_dir_all(&ctx.staging.dir).unwrap();
        drop(Bin::open(&ctx.data_dir).unwrap());
        std::fs::write(ctx.data_dir.join(STORE), b"old store").unwrap();
        std::fs::write(ctx.data_dir.join("lodestar.pre-v5.db"), b"an older one").unwrap();
        keep_previous(&ctx, OLD_MSI);
        // The helper keeps the installed 0.7.0 MSI as `current\`.
        let current = ctx.staging.current();
        std::fs::create_dir_all(&current).unwrap();
        let pins = Pins {
            version: "0.7.0".into(),
            channel: Channel::Stable,
            sha256: Sha256::of(b"new").to_hex(),
            size: 3,
        };
        std::fs::write(current.join(PINS), serde_json::to_vec(&pins).unwrap()).unwrap();
        Env { _tmp: tmp, ctx }
    }

    fn keep_previous(ctx: &Context, msi: &[u8]) {
        let previous = ctx.staging.previous();
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join("lodestar-0.6.0.msi"), OLD_MSI).unwrap();
        let pins = Pins {
            version: "0.6.0".into(),
            channel: Channel::Stable,
            sha256: Sha256::of(msi).to_hex(),
            size: msi.len() as u64,
        };
        std::fs::write(previous.join(PINS), serde_json::to_vec(&pins).unwrap()).unwrap();
    }

    /// The install of 0.7.0 from 0.6.0, its migration and a passed
    /// probation: what leaves the record.
    fn installed_and_passed(env: &Env) {
        let j = installing(&env.ctx, "0123abcd", &v("0.6.0"), &v("0.7.0")).unwrap();
        write(&env.ctx, &j).unwrap();
        // 0.7.0 migrates: the snapshot, then a changed store.
        std::fs::write(env.ctx.data_dir.join(SNAP), b"old store").unwrap();
        std::fs::write(env.ctx.data_dir.join(STORE), b"new store").unwrap();
        let fake = Fake::new(Start::Auto);
        let out = probation(&env.ctx, &fake, j, false);
        assert!(matches!(out, Outcome::Installed { .. }), "{out:?}");
    }

    fn record(env: &Env) -> Option<Installed> {
        layout::read_json(&env.ctx.staging.dir.join(INSTALLED)).unwrap()
    }

    #[test]
    fn a_passed_probation_records_the_install_and_its_snapshot() {
        let env = env();
        installed_and_passed(&env);
        let rec = record(&env).unwrap();
        assert_eq!((rec.from.as_str(), rec.to.as_str()), ("0.6.0", "0.7.0"));
        assert_eq!(rec.snapshot.as_deref(), Some(SNAP));
        assert!(read(&env.ctx).unwrap().is_none(), "the journal went");
        assert_eq!(
            manual_plan(&env.ctx, &v("0.7.0")).unwrap(),
            ManualPlan {
                from: "0.6.0".into(),
                to: "0.7.0".into(),
                snapshot: Some(SNAP.into()),
            }
        );
    }

    #[test]
    fn an_install_that_took_no_snapshot_keeps_the_store() {
        let env = env();
        let j = installing(&env.ctx, "0123abcd", &v("0.6.0"), &v("0.7.0")).unwrap();
        write(&env.ctx, &j).unwrap();
        probation(&env.ctx, &Fake::new(Start::Auto), j, false);
        assert_eq!(record(&env).unwrap().snapshot, None);
        let plan = manual_plan(&env.ctx, &v("0.7.0")).unwrap();
        assert_eq!(plan.snapshot, None);
        let j = manual_journal(&env.ctx, &v("0.7.0"), "ab").unwrap();
        assert_eq!(choose(&env.ctx, &j.snapshots_before).unwrap(), None);
    }

    #[test]
    fn manual_rollback_refuses_what_it_cant_do_safely() {
        let env = env();
        let ctx = &env.ctx;
        let code = |r: Result<ManualPlan, ManualRefusal>| r.unwrap_err().code();
        // Nothing recorded.
        assert_eq!(code(manual_plan(ctx, &v("0.7.0"))), "no_previous_version");
        installed_and_passed(&env);
        // Another version runs (installed by hand since).
        assert_eq!(code(manual_plan(ctx, &v("0.7.1"))), "no_previous_version");
        // A journal is in place.
        let j = installing(ctx, "ffff", &v("0.7.0"), &v("0.8.0")).unwrap();
        write(ctx, &j).unwrap();
        assert_eq!(code(manual_plan(ctx, &v("0.7.0"))), "busy");
        clear(ctx);
        // The snapshot the install took is gone: restoring nothing would
        // hand 0.6.0 a store it can't read.
        let snap = ctx.data_dir.join(SNAP);
        std::fs::rename(&snap, ctx.data_dir.join("aside")).unwrap();
        assert_eq!(code(manual_plan(ctx, &v("0.7.0"))), "snapshot_missing");
        std::fs::rename(ctx.data_dir.join("aside"), &snap).unwrap();
        // The MSI changed since it was pinned.
        std::fs::write(
            ctx.staging.previous().join("lodestar-0.6.0.msi"),
            b"changed",
        )
        .unwrap();
        assert_eq!(code(manual_plan(ctx, &v("0.7.0"))), "no_previous_version");
        // Gone.
        std::fs::remove_dir_all(ctx.staging.previous()).unwrap();
        assert_eq!(code(manual_plan(ctx, &v("0.7.0"))), "no_previous_version");
        keep_previous(ctx, OLD_MSI);
        // `current\` isn't the running version: the record is stale.
        let current = ctx.staging.current().join(PINS);
        let kept = std::fs::read(&current).unwrap();
        std::fs::remove_file(&current).unwrap();
        assert_eq!(code(manual_plan(ctx, &v("0.7.0"))), "no_previous_version");
        std::fs::write(&current, kept).unwrap();
        std::fs::remove_dir_all(ctx.staging.previous()).unwrap();
        assert_eq!(code(manual_plan(ctx, &v("0.7.0"))), "no_previous_version");
        keep_previous(ctx, OLD_MSI);
        assert!(manual_plan(ctx, &v("0.7.0")).is_ok());
    }

    #[test]
    fn a_damaged_record_refuses() {
        let env = env();
        keep_previous(&env.ctx, OLD_MSI);
        let path = env.ctx.staging.dir.join(INSTALLED);
        let good = Installed {
            format: FORMAT,
            from: "0.6.0".into(),
            to: "0.7.0".into(),
            snapshot: None,
            start_before: None,
        };
        let bad = [
            Installed {
                format: FORMAT + 1,
                ..good.clone()
            },
            Installed {
                from: "0.7.0".into(),
                ..good.clone()
            },
            Installed {
                from: "0.8.0".into(),
                ..good.clone()
            },
            Installed {
                from: "not a version".into(),
                ..good.clone()
            },
            Installed {
                snapshot: Some("../lodestar.pre-v8.db".into()),
                ..good.clone()
            },
            Installed {
                snapshot: Some("lodestar.db".into()),
                ..good.clone()
            },
            Installed {
                snapshot: Some("lodestar.before-rollback-20261112-101010.db".into()),
                ..good.clone()
            },
        ];
        for rec in bad {
            std::fs::write(&path, serde_json::to_vec(&rec).unwrap()).unwrap();
            assert!(manual_plan(&env.ctx, &v("0.7.0")).is_err(), "{rec:?}");
        }
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(manual_plan(&env.ctx, &v("0.7.0")).unwrap_err().code(), "io");
        std::fs::write(&path, serde_json::to_vec(&good).unwrap()).unwrap();
        assert!(manual_plan(&env.ctx, &v("0.7.0")).is_ok());
    }

    #[test]
    fn the_manual_journal_restores_exactly_the_recorded_snapshot() {
        let env = env();
        installed_and_passed(&env);
        // Snapshots taken since, older and newer, aren't the one.
        std::fs::write(env.ctx.data_dir.join("lodestar.pre-v3.db"), b"x").unwrap();
        std::fs::write(env.ctx.data_dir.join("lodestar.pre-v9.db"), b"y").unwrap();
        let j = manual_journal(&env.ctx, &v("0.7.0"), "0123456789abcdef").unwrap();
        assert!(is_manual(&j));
        assert_eq!(j.id, "manual-0123456789abcdef");
        assert_eq!((j.from.as_str(), j.to.as_str()), ("0.6.0", "0.7.0"));
        let chosen = choose(&env.ctx, &j.snapshots_before).unwrap().unwrap();
        assert_eq!(chosen.name, SNAP);
        for bad in [
            "",
            "ABC",
            "0123456789abcdef0123456789abcdef0",
            "../x",
            "12 3",
        ] {
            assert_eq!(
                manual_journal(&env.ctx, &v("0.7.0"), bad)
                    .unwrap_err()
                    .code(),
                "io",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_manual_rollback_restores_the_store_and_reinstalls_the_previous_msi() {
        let env = env();
        installed_and_passed(&env);
        let fake = Fake::new(Start::Manual);
        let out = manual(&env.ctx, &fake, &v("0.7.0"), "0123abcd").unwrap();
        assert_eq!(
            out,
            Outcome::RolledBack {
                failed: "0.7.0".into(),
                to: "0.6.0".into(),
                healthy: true,
            }
        );
        let calls = fake.calls();
        assert!(
            calls.contains(&"msiexec 0.6.0=true".to_string()),
            "{calls:?}"
        );
        // The start type now is what goes back, after the disabled steps.
        assert_eq!(
            calls.iter().rfind(|c| c.starts_with("set_start")).unwrap(),
            "set_start Manual"
        );
        assert_eq!(calls.last().unwrap(), "start");
        assert!(!calls.iter().any(|c| c.starts_with("event")), "{calls:?}");
        let store = std::fs::read(env.ctx.data_dir.join(STORE)).unwrap();
        assert_eq!(store, b"old store");
        assert!(!env.ctx.data_dir.join(SNAP).exists());
        assert!(!env.ctx.data_dir.join("bin").join(MARKER).exists());
        assert!(read(&env.ctx).unwrap().is_none(), "the journal went");
        assert!(record(&env).is_none(), "nothing left to roll back by hand");
        let skipped: serde_json::Value = Bin::open(&env.ctx.data_dir)
            .unwrap()
            .read(super::super::check::SKIPPED_FILE)
            .unwrap()
            .unwrap();
        assert_eq!(skipped[0]["version"], "0.7.0", "{skipped}");
        assert_eq!(skipped[0]["reason"], SKIP_MANUAL, "{skipped}");
        // The reinstalled MSI is current again.
        let pins: Pins = layout::read_json(&env.ctx.staging.current().join(PINS))
            .unwrap()
            .unwrap();
        assert_eq!(pins.version, "0.6.0");
        // Once 0.6.0 runs, there's nothing to roll back.
        assert!(manual_plan(&env.ctx, &v("0.6.0")).is_err());
    }

    #[test]
    fn a_snapshot_purged_before_the_service_stopped_undoes_the_rollback() {
        let env = env();
        installed_and_passed(&env);
        let j = manual_journal(&env.ctx, &v("0.7.0"), "ab").unwrap();
        write(&env.ctx, &j).unwrap();
        // The service's retention purge, between the plan and the stop.
        std::fs::remove_file(env.ctx.data_dir.join(SNAP)).unwrap();
        let fake = Fake::new(Start::Manual);
        let out = roll_back(&env.ctx, &fake, j);
        let Outcome::RollbackHalted { why, recovery } = out else {
            panic!("{out:?}");
        };
        assert!(why.contains("is gone"), "{why}");
        assert!(recovery.contains("couldn't start"), "{recovery}");
        // Nothing was changed: the store, no marker, no skip, no journal,
        // and the service started again as it was.
        let store = std::fs::read(env.ctx.data_dir.join(STORE)).unwrap();
        assert_eq!(store, b"new store");
        assert!(!env.ctx.data_dir.join("bin").join(MARKER).exists());
        let skipped: Option<serde_json::Value> = Bin::open(&env.ctx.data_dir)
            .unwrap()
            .read(super::super::check::SKIPPED_FILE)
            .unwrap();
        assert!(skipped.is_none(), "{skipped:?}");
        assert!(read(&env.ctx).unwrap().is_none());
        let calls = fake.calls();
        assert!(!calls.iter().any(|c| c.starts_with("msiexec")), "{calls:?}");
        assert_eq!(
            calls[..4],
            ["set_start Disabled", "stop", "set_start Auto", "start"],
            "{calls:?}"
        );
    }

    #[test]
    fn a_manual_rollback_without_its_msi_changes_nothing() {
        let env = env();
        installed_and_passed(&env);
        let j = manual_journal(&env.ctx, &v("0.7.0"), "ab").unwrap();
        write(&env.ctx, &j).unwrap();
        std::fs::remove_dir_all(env.ctx.staging.previous()).unwrap();
        let fake = Fake::new(Start::Manual);
        let out = roll_back(&env.ctx, &fake, j);
        assert!(matches!(out, Outcome::RollbackHalted { .. }), "{out:?}");
        // Not touched: the journal goes and the service was never stopped.
        assert!(read(&env.ctx).unwrap().is_none());
        let calls = fake.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c == "stop" || c.starts_with("set_start")),
            "{calls:?}"
        );
    }

    fn skipped(env: &Env) -> Option<serde_json::Value> {
        Bin::open(&env.ctx.data_dir)
            .unwrap()
            .read(super::super::check::SKIPPED_FILE)
            .unwrap()
    }

    /// A crash or restart after the journal was written and before step 1
    /// ran: the next run's `resume` finishes it as a manual one.
    #[test]
    fn a_manual_journal_written_before_a_crash_is_finished_by_resume() {
        let env = env();
        installed_and_passed(&env);
        let j = manual_journal(&env.ctx, &v("0.7.0"), "ab").unwrap();
        write(&env.ctx, &j).unwrap();
        let fake = Fake::new(Start::Manual);
        let out = resume(&env.ctx, &fake, &v("0.7.0")).unwrap().unwrap();
        assert!(matches!(out, Outcome::RolledBack { .. }), "{out:?}");
        assert_eq!(
            std::fs::read(env.ctx.data_dir.join(STORE)).unwrap(),
            b"old store"
        );
        assert_eq!(skipped(&env).unwrap()[0]["reason"], SKIP_MANUAL);
        assert!(read(&env.ctx).unwrap().is_none());
        assert!(record(&env).is_none());
    }

    /// Resumed after step 2 (the snapshot chosen) and after step 4 (the
    /// store restored, the MSI reinstalled): each finishes, from where it
    /// stood.
    #[test]
    fn a_manual_rollback_resumes_after_steps_2_and_4() {
        for done in [2u8, 4] {
            let env = env();
            installed_and_passed(&env);
            let mut j = manual_journal(&env.ctx, &v("0.7.0"), "ab").unwrap();
            let snap = choose(&env.ctx, &j.snapshots_before).unwrap();
            assert_eq!(snap.as_ref().map(|s| s.name.as_str()), Some(SNAP));
            j.touched = true;
            j.state = State::RollingBack {
                snapshot: snap,
                done,
            };
            // What step 1 left.
            Bin::open(&env.ctx.data_dir)
                .unwrap()
                .write(MARKER, &serde_json::json!({ "windows_rollback": "0.7.0" }))
                .unwrap();
            if done == 4 {
                // Step 3's restore, and step 4's reinstall of 0.6.0.
                std::fs::rename(env.ctx.data_dir.join(SNAP), env.ctx.data_dir.join(STORE)).unwrap();
            }
            write(&env.ctx, &j).unwrap();
            let running = if done == 4 { "0.6.0" } else { "0.7.0" };
            let fake = Fake::new(Start::Manual);
            let out = resume(&env.ctx, &fake, &v(running)).unwrap().unwrap();
            assert!(matches!(out, Outcome::RolledBack { .. }), "{done}: {out:?}");
            let ran = fake.calls().iter().any(|c| c.starts_with("msiexec"));
            assert_eq!(ran, done < 4, "{done}: {:?}", fake.calls());
            assert_eq!(
                std::fs::read(env.ctx.data_dir.join(STORE)).unwrap(),
                b"old store",
                "{done}"
            );
            assert!(
                !env.ctx.data_dir.join("bin").join(MARKER).exists(),
                "{done}"
            );
            assert!(read(&env.ctx).unwrap().is_none(), "{done}");
            assert!(record(&env).is_none(), "{done}");
        }
    }

    /// The next install that passes replaces the record.
    #[test]
    fn a_later_install_that_passes_replaces_the_record() {
        let env = env();
        installed_and_passed(&env);
        let j = installing(&env.ctx, "0123abce", &v("0.7.0"), &v("0.8.0")).unwrap();
        write(&env.ctx, &j).unwrap();
        probation(&env.ctx, &Fake::new(Start::Auto), j, false);
        let rec = record(&env).unwrap();
        assert_eq!((rec.from.as_str(), rec.to.as_str()), ("0.7.0", "0.8.0"));
        // It took no snapshot: the 0.7.0 one is an older install's.
        assert_eq!(rec.snapshot, None);
    }

    /// A newer install that fails probation is rolled back automatically:
    /// the record goes with it, since `previous\` no longer holds its MSI.
    #[test]
    fn an_automatic_rollback_removes_the_record() {
        let env = env();
        installed_and_passed(&env);
        // The helper kept 0.7.0's MSI as `current\` (its pins are `env`'s).
        std::fs::write(env.ctx.staging.current().join("lodestar-0.7.0.msi"), b"new").unwrap();
        let j = installing(&env.ctx, "0123abce", &v("0.7.0"), &v("0.8.0")).unwrap();
        write(&env.ctx, &j).unwrap();
        let fake = Fake {
            unhealthy: true,
            ..Fake::new(Start::Auto)
        };
        let out = probation(&env.ctx, &fake, j, false);
        assert!(matches!(out, Outcome::RolledBack { .. }), "{out:?}");
        assert!(record(&env).is_none());
        assert_eq!(skipped(&env).unwrap()[0]["reason"], "failed");
    }

    /// A record that can't be written leaves none behind: a stale one from
    /// an earlier install mustn't stay and match later.
    #[test]
    fn a_record_that_cant_be_written_leaves_none() {
        let env = env();
        installed_and_passed(&env);
        assert!(record(&env).is_some());
        // A folder where the record's temporary file goes makes the write
        // fail; then `keep_installed` removes the record it can.
        let j = installing(&env.ctx, "0123abce", &v("0.7.0"), &v("0.8.0")).unwrap();
        let path = env.ctx.staging.dir.join(INSTALLED);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        keep_installed(&env.ctx, &j);
        // The folder stays (it isn't a file to remove), and a record that
        // can't be read refuses.
        assert_eq!(manual_plan(&env.ctx, &v("0.8.0")).unwrap_err().code(), "io");
        std::fs::remove_dir(&path).unwrap();
        // An older record, and a listing that fails: removed.
        installed_and_passed_again(&env);
    }

    fn installed_and_passed_again(env: &Env) {
        let j = installing(&env.ctx, "0123abcf", &v("0.6.0"), &v("0.7.0")).unwrap();
        keep_installed(&env.ctx, &j);
        assert!(record(env).is_some());
        // The store's folder can't be listed: no record.
        let data = env.ctx.data_dir.clone();
        let aside = data.with_extension("aside");
        std::fs::rename(&data, &aside).unwrap();
        std::fs::write(&data, b"not a folder").unwrap();
        keep_installed(&env.ctx, &j);
        assert!(record(env).is_none());
        std::fs::remove_file(&data).unwrap();
        std::fs::rename(&aside, &data).unwrap();
    }

    #[test]
    fn a_manual_rollbacks_halt_says_it_was_manual() {
        let env = env();
        installed_and_passed(&env);
        let j = manual_journal(&env.ctx, &v("0.7.0"), "ab").unwrap();
        let event = recovery_event(&j, 2, Cause::Step);
        assert!(
            event.starts_with(
                "Lodestar's manual rollback from 0.7.0 to 0.6.0 stopped after step 2 of 5."
            ),
            "{event}"
        );
        assert!(!event.contains("health check"), "{event}");
        let text = recovery(&env.ctx, &j, 0, "why", Cause::Step);
        assert!(
            text.starts_with("Lodestar's manual rollback from 0.7.0 to 0.6.0 couldn't start: why."),
            "{text}"
        );
        let failed = Journal {
            id: "ab".into(),
            ..j
        };
        assert!(recovery_event(&failed, 2, Cause::Step).starts_with(
            "Lodestar 0.7.0 failed its health check, and the rollback to 0.6.0 stopped after step 2 of 5."
        ));
    }
}
