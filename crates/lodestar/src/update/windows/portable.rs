//! The portable build's own update and rollback (PK-win-portable-upd;
//! architecture §12.2 "Windows portable", plan
//! `docs/plan/2026-11-06-m6-gap-audit.md` §4.3).
//!
//! It runs as the user who unpacked the zip: no helper, no new rights.
//! The program is `<folder>\lodestar.exe` and the data folder
//! `<folder>\data` (what `Start Lodestar.cmd` sets, and what
//! [`super::check::detect`] requires), so `<data>\bin` is on the same
//! volume and every move below is a rename.
//!
//! **`<data>\bin` must be private** before anything is written or run
//! from it ([`Sys::bin_private`]): owned by this user, SYSTEM or
//! Administrators, and its DACL letting in only those
//! (`windows::acl::log_folder`'s rule). A missing one is created with that
//! protected DACL first ([`ensure_bin`], before the notifier's first
//! open); one that isn't private is refused (`bin_not_private`), never
//! repaired.
//!
//! **The journal** is `<data>\bin\portable.json`, written by temporary
//! file and rename before and after each step, and parsed as untrusted
//! input (capped, then checked against this program's own path, version
//! and the files' SHA-256). Each step can be repeated, and every state
//! but one moment leaves a runnable `lodestar.exe` in place:
//!
//! 1. **Staged** ([`install`]): the downloaded zip is hashed and extracted
//!    from one handle (so the bytes checked are the bytes unpacked), with
//!    `lodestar-companions`' bounded extractor, into `bin\portable-staging`;
//!    its `lodestar.exe` becomes `bin\next.exe`, and its SHA-256 is
//!    recorded, with the running program's and the store's snapshots.
//! 2. **Swapping:** the running `lodestar.exe` is renamed to
//!    `bin\previous.exe` (Windows allows renaming a running program), then
//!    `next.exe` into its place. If the second rename fails, the first is
//!    undone.
//! 3. **Swapped:** the caller relaunches (PK-win-restart). The new version
//!    counts an attempt at each start ([`begin`]) and must pass the
//!    self-test within [`PROBATION`] of its services starting
//!    ([`self_test`]). Passed: `previous.exe` and the journal go.
//! 4. **Rolling back** (the self-test failed or timed out, the start
//!    failed, or a start past [`MAX_ATTEMPTS`]): the new program is renamed
//!    to `bin\failed.exe`, `previous.exe` back to `lodestar.exe`, and the
//!    old version is relaunched. At its start, before anything opens the
//!    store, it restores the failed version's earliest pre-migration
//!    snapshot (if it took one), marks the version `failed` in `skipped`,
//!    and clears the journal.
//!
//! **The one gap:** between the two renames of a swap or a rollback there
//! is no `lodestar.exe`. Everything slow comes first (the file that moves
//! in is hashed and held open, unwritable, before anything moves), so the
//! renames run back to back. A power cut in that moment leaves the program
//! that's needed in `data\bin\previous.exe`; once it's back as
//! `lodestar.exe`, the next start finishes the step by the hashes.
//!
//! Which program runs is never taken from the journal: each start
//! compares its own version and the SHA-256 of what's at
//! `lodestar.exe` with the journal's, and relaunches when they differ.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestar_log::log_event;
use lodestar_store::snapshots::{self, Kind as SnapKind};
use lodestar_update::{Pin, Sha256};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::check::{SKIP_FAILED, add_skip};
use super::history;
use crate::update::layout::{self, Bin, Lock};
use crate::update::log_events::{
    UPDATE_INSTALL_FAILED, UPDATE_PROBATION_FAILED, UPDATE_PROBATION_PASSED, UPDATE_STATE_UNSAVED,
};

/// In `<data>\bin`.
pub const JOURNAL: &str = "portable.json";
pub const NEXT: &str = "next.exe";
pub const PREVIOUS: &str = "previous.exe";
pub const FAILED: &str = "failed.exe";
pub const STAGING: &str = "portable-staging";
/// The program's name beside the launcher.
pub const PROGRAM: &str = "lodestar.exe";
const FORMAT: u32 = 1;
/// Starts the new version gets before it's rolled back (§12.2).
pub const MAX_ATTEMPTS: u32 = 3;
/// How long the new version has to pass its self-test once its services
/// have started, migrations included (§12.2's 5 minutes).
pub const PROBATION: Duration = Duration::from_secs(5 * 60);

/// The journal. Unknown fields are accepted, because after a rollback
/// the older version reads what the newer one wrote; a field an older
/// version mustn't ignore bumps [`FORMAT`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub format: u32,
    /// The version installed from.
    pub from: String,
    /// The version installed.
    pub to: String,
    /// SHA-256 of `from`'s `lodestar.exe`.
    pub old_sha256: String,
    /// SHA-256 of `to`'s `lodestar.exe`, from the verified zip.
    pub new_sha256: String,
    /// The store's snapshots before the install (names).
    #[serde(default)]
    pub snapshots_before: Vec<String>,
    /// Starts of `to` so far.
    #[serde(default)]
    pub attempts: u32,
    /// The rollback's store step, once chosen (before anything is
    /// removed): `Some(None)` when the failed version took no snapshot.
    /// Recorded so a repeat after a crash restores the same one, never the
    /// next one along.
    #[serde(default, with = "chosen")]
    pub restore: Option<Option<Snapshot>>,
    pub state: State,
    /// This attempt's own id (random), so each install of the same build
    /// is its own history entry. Absent in a journal from before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
}

/// A snapshot the rollback restores: its name beside the store and its
/// SHA-256 when it was chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub name: String,
    pub sha256: String,
}

/// `restore` on disk: absent (not chosen), `{"none": true}` or the
/// snapshot, so "chosen: none" and "not chosen" never read the same.
mod chosen {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::Snapshot;

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Chosen {
        None,
        Snapshot(Snapshot),
    }

    pub fn serialize<S: Serializer>(v: &Option<Option<Snapshot>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            None => s.serialize_none(),
            Some(None) => Some(Chosen::None).serialize(s),
            Some(Some(snap)) => Some(Chosen::Snapshot(snap.clone())).serialize(s),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<Snapshot>>, D::Error> {
        Ok(match Option::<Chosen>::deserialize(d)? {
            None => None,
            Some(Chosen::None) => Some(None),
            Some(Chosen::Snapshot(snap)) => Some(Some(snap)),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Staged,
    Swapping,
    Swapped,
    RollingBack,
}

/// Where a step can fail, for the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    /// After the zip is extracted, before `next.exe` is in place.
    Extracted,
    /// Before the running program is renamed aside.
    Aside,
    /// Between the two renames.
    Between,
    /// Before the journal records `Swapped`.
    Recorded,
    /// Before the files are swapped back on a rollback.
    Back,
    /// Before the old program is renamed back into place.
    PutBack,
    /// Before the store's side files are removed.
    Sides,
    /// After the snapshot is renamed over the store, before the journal
    /// goes.
    Restored,
}

/// The system side.
pub trait Sys {
    /// This program as it was started: `<folder>\lodestar.exe`.
    fn program(&self) -> PathBuf;
    /// This build's version.
    fn version(&self) -> String;
    /// `Ok` when the folder is private (see the module's note).
    fn bin_private(&self, dir: &Path) -> Result<(), String>;
    /// Creates the folder with the private, protected DACL.
    fn create_bin(&self, dir: &Path) -> io::Result<()>;
    /// Restarts this run into the program now in place (Restart now's
    /// relaunch); an error means it keeps running.
    fn restart(&self) -> io::Result<()>;
    /// A test's failure at `point`.
    fn fault(&self, _point: Point) -> io::Result<()> {
        Ok(())
    }
}

/// What the caller does after [`begin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Nothing to do: start as usual.
    Normal,
    /// This is the new version on probation: start, then [`self_test`].
    Probation,
    /// Another program is the one to run now: start `lodestar.exe` as a
    /// new copy and exit without serving.
    Relaunch,
    /// The rollback's store couldn't be restored: neither version starts
    /// (§12.2), and the journal stays for the next start.
    Refuse,
}

/// Creates `<data>\bin` with the private DACL if it's missing, before
/// anything else creates it with the inherited one. An existing folder is
/// left as it is (and judged at install).
pub fn ensure_bin(sys: &dyn Sys, data_dir: &Path) -> io::Result<()> {
    let dir = data_dir.join("bin");
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => sys.create_bin(&dir),
        Err(e) => Err(e),
        Ok(_) => Ok(()),
    }
}

/// Why this installation can't update itself, or `None`: `<data>\bin`
/// private, the data folder beside the program, and no update under way.
pub fn cannot_install(sys: &dyn Sys, data_dir: &Path, bin: &Bin) -> Option<&'static str> {
    if !beside(sys, data_dir) {
        return Some("install_kind_unknown");
    }
    if sys.bin_private(&data_dir.join("bin")).is_err() {
        return Some("bin_not_private");
    }
    if bin.exists(JOURNAL) {
        return Some("update_in_progress");
    }
    None
}

/// The data folder is `<program's folder>\data`.
fn beside(sys: &dyn Sys, data_dir: &Path) -> bool {
    let program = sys.program();
    program
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(PROGRAM))
        && program
            .parent()
            .is_some_and(|f| same(&f.join("data"), data_dir))
}

fn same(a: &Path, b: &Path) -> bool {
    // As `check::detect` compares them: either separator, any case, no
    // `\\?\` prefix, no trailing separator.
    let n = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
        s.trim_end_matches('\\').to_string()
    };
    n(a) == n(b)
}

/// Steps 1 and 2: from the downloaded zip at `part` (already checked
/// against `pin` while it downloaded, checked again here) to the new
/// program in place and the journal at `Swapped`. The caller holds
/// `<data>\bin\.lock` and relaunches on `Ok`. On `Err` the running
/// program is back in place and nothing else is left.
pub fn install(
    sys: &dyn Sys,
    data_dir: &Path,
    db: &Path,
    bin: &Bin,
    part: &Path,
    pin: &Pin,
) -> Result<(), &'static str> {
    if let Some(why) = cannot_install(sys, data_dir, bin) {
        return Err(why);
    }
    let from = sys.version();
    let to = pin.version.to_string();
    let result = stage(sys, bin, part, pin).and_then(|new_sha256| {
        let program = sys.program();
        let (old, _) = hash(&program).map_err(|_| "program_unreadable")?;
        let snapshots_before = snapshots::list(db)
            .map_err(|_| "snapshots_unlisted")?
            .into_iter()
            .map(|s| s.name)
            .collect();
        let mut j = Journal {
            format: FORMAT,
            from,
            to,
            old_sha256: old.to_hex(),
            new_sha256,
            snapshots_before,
            attempts: 0,
            restore: None,
            state: State::Staged,
            attempt: getrandom::u64().ok(),
        };
        write(bin, &j)?;
        swap(sys, bin, &mut j)
    });
    let _ = std::fs::remove_file(part);
    if let Err(code) = result {
        log_event!(UPDATE_INSTALL_FAILED, error = code);
    }
    result
}

/// Step 1: `bin\next.exe` from the zip; its SHA-256.
fn stage(sys: &dyn Sys, bin: &Bin, part: &Path, pin: &Pin) -> Result<String, &'static str> {
    let staging = bin.path(STAGING);
    remove_tree(&staging).map_err(|_| "staging_in_the_way")?;
    let _ = std::fs::remove_file(bin.path(NEXT));
    let mut zip = open_held(part).map_err(|_| "download_missing")?;
    let (sha, size) = hash_from(&zip).map_err(|_| "download_unreadable")?;
    if sha != pin.sha256 || size != pin.size {
        return Err("verify_failed");
    }
    zip.seek(SeekFrom::Start(0))
        .map_err(|_| "download_unreadable")?;
    let inner = format!("lodestar-{}-x86_64-windows-portable/{PROGRAM}", pin.version);
    let exe = lodestar_companions::catalogue::RelPath::parse(&inner).map_err(|_| "bad_version")?;
    lodestar_companions::extract::extract(
        lodestar_companions::catalogue::ArchiveKind::Zip,
        &mut zip,
        &exe,
        &staging,
    )
    .map_err(|_| "zip_refused")?;
    let staged = staging.join(inner.replace('/', std::path::MAIN_SEPARATOR_STR));
    let moved = sys
        .fault(Point::Extracted)
        .and_then(|()| layout::rename_retrying(&staged, &bin.path(NEXT)));
    let _ = remove_tree(&staging);
    moved.map_err(|_| "stage_failed")?;
    match hash(&bin.path(NEXT)) {
        Ok((sha, _)) => Ok(sha.to_hex()),
        Err(_) => {
            let _ = std::fs::remove_file(bin.path(NEXT));
            Err("stage_failed")
        }
    }
}

/// Step 2: the running program aside, the new one in its place, the
/// journal at `Swapped`. Any failure puts the running program back.
fn swap(sys: &dyn Sys, bin: &Bin, j: &mut Journal) -> Result<(), &'static str> {
    let program = sys.program();
    let (next, previous) = (bin.path(NEXT), bin.path(PREVIOUS));
    let undo = |j: &Journal| {
        if put_back(sys, &program, &previous, &j.old_sha256).is_ok_and(|b| b) {
            let _ = std::fs::remove_file(&next);
            clear(bin);
            return;
        }
        // The old program can't go back: the verified new one is better
        // than none. The journal stays, so the next start judges it (it's
        // the new version's probation, or puts the old one back).
        if std::fs::symlink_metadata(&program).is_err() && is(&next, &j.new_sha256) {
            let _ = layout::rename_retrying(&next, &program);
        }
    };
    j.state = State::Swapping;
    if write(bin, j).is_err() || sys.fault(Point::Aside).is_err() {
        undo(j);
        return Err("swap_failed");
    }
    // Checked again before anything moves, through a handle that lets
    // nobody write to it until both renames are done, so they run back to
    // back: the only moment with no `lodestar.exe` is between two renames
    // in the same folder tree (see the module's note on that gap).
    let Some(held) = held_if(&next, &j.new_sha256) else {
        undo(j);
        return Err("swap_failed");
    };
    let _ = std::fs::remove_file(&previous);
    if layout::rename_retrying(&program, &previous).is_err() {
        drop(held);
        undo(j);
        return Err("swap_failed");
    }
    if sys.fault(Point::Between).is_err() || layout::rename_retrying(&next, &program).is_err() {
        drop(held);
        undo(j);
        return Err("swap_failed");
    }
    drop(held);
    // And once it's there: what was checked is what was moved.
    if !is(&program, &j.new_sha256) {
        let _ = layout::rename_retrying(&program, &next);
        undo(j);
        return Err("swap_failed");
    }
    j.state = State::Swapped;
    if sys.fault(Point::Recorded).is_err() || write(bin, j).is_err() {
        // The new program is in place but unrecorded: undo it rather than
        // relaunch into a version no journal follows.
        let _ = layout::rename_retrying(&program, &next);
        undo(j);
        return Err("swap_failed");
    }
    Ok(())
}

/// The old program back at `program` from `previous` (only if it's the
/// one recorded), unless it's there already. `Ok(true)` when it's in
/// place.
fn put_back(sys: &dyn Sys, program: &Path, previous: &Path, old_sha256: &str) -> io::Result<bool> {
    if is(program, old_sha256) {
        let _ = std::fs::remove_file(previous);
        return Ok(true);
    }
    if std::fs::symlink_metadata(program).is_ok() {
        return Ok(false);
    }
    // Checked through a handle nobody can write through until it's moved,
    // and checked again once it's there, as `swap` does.
    let Some(held) = held_if(previous, old_sha256) else {
        return Ok(false);
    };
    sys.fault(Point::PutBack)?;
    let moved = layout::rename_retrying(previous, program);
    drop(held);
    moved?;
    if !is(program, old_sha256) {
        let _ = layout::rename_retrying(program, previous);
        return Ok(false);
    }
    Ok(true)
}

/// `path` opened with [`open_held`] when it has this SHA-256: while the
/// handle lives, nobody can write to it (on Windows).
fn held_if(path: &Path, sha256: &str) -> Option<File> {
    open_held(path)
        .ok()
        .filter(|f| hash_from(f).is_ok_and(|(s, _)| s.to_hex() == sha256))
}

/// Whether `path` is a regular file with this SHA-256.
fn is(path: &Path, sha256: &str) -> bool {
    hash(path).is_ok_and(|(s, _)| s.to_hex() == sha256)
}

/// At every start, before anything opens the store or `<data>\bin` is
/// otherwise used: finishes whatever step the journal is at. [`begin`]
/// without its lock, for the tests.
#[cfg(test)]
pub fn at_start(sys: &dyn Sys, data_dir: &Path, db: &Path) -> Start {
    begin(sys, data_dir, db).0
}

/// At every start, before anything opens the store or `<data>\bin` is
/// otherwise used: finishes whatever step the journal is at, with
/// `<data>\bin\.lock` kept for [`Start::Probation`]:
/// the caller holds it until the self-test decides ([`passed`] or
/// [`failed`]), so no second copy can count an attempt or roll back
/// meanwhile, even while this one is still starting (the notifier takes
/// the lock only later, and retries once it's free).
pub fn begin(sys: &dyn Sys, data_dir: &Path, db: &Path) -> (Start, Option<Lock>) {
    if !beside(sys, data_dir) {
        return (Start::Normal, None);
    }
    let Ok(Some(bin)) = Bin::existing(data_dir) else {
        return (Start::Normal, None);
    };
    if !bin.exists(JOURNAL) {
        return (Start::Normal, None);
    }
    // Another copy is running here (on probation, holding it from here to
    // its self-test; or its notifier, holding it for its whole life): this
    // start judges nothing, so a second start can't count an attempt or
    // roll back a healthy update.
    let Ok(Some(lock)) = bin.lock() else {
        return (Start::Normal, None);
    };
    let start = judge(sys, &bin, db, &data_dir.join("bin"));
    let lock = (start == Start::Probation).then_some(lock);
    (start, lock)
}

/// The journal's step, under the lock [`begin`] holds.
fn judge(sys: &dyn Sys, bin: &Bin, db: &Path, bin_dir: &Path) -> Start {
    // A journal in a folder others can write could have been planted:
    // nothing is moved or run on its say.
    if sys.bin_private(bin_dir).is_err() {
        log_event!(UPDATE_INSTALL_FAILED, error = "bin_not_private");
        return Start::Normal;
    }
    let mut j = match read(bin) {
        Ok(Some(j)) => j,
        Ok(None) => return Start::Normal,
        Err(e) => {
            // Left for someone to look at; Install refuses meanwhile.
            log_event!(
                UPDATE_STATE_UNSAVED,
                file = JOURNAL,
                error = lodestar_log::Error::io(&e)
            );
            return Start::Normal;
        }
    };
    let program = sys.program();
    match j.state {
        State::Staged => {
            let _ = std::fs::remove_file(bin.path(NEXT));
            clear(bin);
            Start::Normal
        }
        State::Swapping => {
            if is(&program, &j.new_sha256) {
                j.state = State::Swapped;
                return swapped(sys, bin, db, j);
            }
            // The program in place was the old one, or is again: the
            // install never finished. A start can only come from
            // `lodestar.exe`, so this is the old version or a stray one,
            // and either way it carries on (never relaunching).
            if put_back(sys, &program, &bin.path(PREVIOUS), &j.old_sha256).is_ok_and(|b| b) {
                let _ = std::fs::remove_file(bin.path(NEXT));
                clear(bin);
            }
            Start::Normal
        }
        State::Swapped => swapped(sys, bin, db, j),
        State::RollingBack => rolling_back(sys, bin, db, j),
    }
}

/// `Swapped` at a start: the new version counts an attempt, or past
/// [`MAX_ATTEMPTS`] rolls back; anything else is rolled back.
fn swapped(sys: &dyn Sys, bin: &Bin, db: &Path, mut j: Journal) -> Start {
    let start = if sys.version() != j.to || !is(&sys.program(), &j.new_sha256) {
        roll_back(sys, bin, j, "not_started")
    } else {
        j.attempts = j.attempts.saturating_add(1);
        if j.attempts > MAX_ATTEMPTS {
            roll_back(sys, bin, j, "attempts")
        } else if write(bin, &j).is_err() {
            // An attempt that can't be counted can't end: roll back.
            roll_back(sys, bin, j, "attempt_unrecorded")
        } else {
            return Start::Probation;
        }
    };
    // The old version is the one starting: it finishes the rollback now.
    match read(bin) {
        Ok(Some(j)) if start == Start::Normal && j.state == State::RollingBack => {
            rolling_back(sys, bin, db, j)
        }
        _ => start,
    }
}

/// Step 4's file half, then a relaunch of the old version (or, when this
/// is it, the rest of step 4 at once).
fn roll_back(sys: &dyn Sys, bin: &Bin, mut j: Journal, why: &'static str) -> Start {
    log_event!(UPDATE_PROBATION_FAILED, check = why);
    j.state = State::RollingBack;
    if write(bin, &j).is_err() {
        // Still `Swapped` on disk: the files are left, and the next start
        // tries again.
        return Start::Normal;
    }
    match files_back(sys, bin, &j) {
        Ok(moved) => relaunch_if(moved, &sys.version(), &j.from),
        Err(_) => Start::Normal,
    }
}

/// The new program to `failed.exe`, the old one back in place. `true`
/// when this moved the new program out of `lodestar.exe` (so the program
/// now there isn't the one running, if the running one was the new).
fn files_back(sys: &dyn Sys, bin: &Bin, j: &Journal) -> io::Result<bool> {
    let program = sys.program();
    sys.fault(Point::Back)?;
    let (previous, failed) = (bin.path(PREVIOUS), bin.path(FAILED));
    if is(&program, &j.new_sha256) {
        // The old program is checked and held first, so the two renames
        // run back to back and the new program never leaves without the
        // old one ready to take its place.
        let Some(held) = held_if(&previous, &j.old_sha256) else {
            return Err(io::Error::other("the previous program isn't there"));
        };
        let _ = std::fs::remove_file(&failed);
        layout::rename_retrying(&program, &failed)?;
        let back = sys
            .fault(Point::PutBack)
            .and_then(|()| layout::rename_retrying(&previous, &program));
        drop(held);
        if let Err(e) = back {
            // Never leave `lodestar.exe` empty: the new program goes back
            // until the next start can try again.
            let _ = layout::rename_retrying(&failed, &program);
            return Err(e);
        }
        if !is(&program, &j.old_sha256) {
            let _ = layout::rename_retrying(&program, &previous);
            let _ = layout::rename_retrying(&failed, &program);
            return Err(io::Error::other("the previous program changed"));
        }
        return Ok(true);
    }
    // The new program isn't in place: put the old one back if it's not.
    match put_back(sys, &program, &previous, &j.old_sha256) {
        Ok(true) => Ok(false),
        Ok(false) => Err(io::Error::other("the previous program isn't there")),
        Err(e) => Err(e),
    }
}

/// `Relaunch` only when this start moved the new program out and this
/// build isn't the old one. The old program (the one with `old_sha256`,
/// which reported `from` when it installed) never relaunches, and a start
/// that moved nothing never does, so two programs can't hand over to each
/// other in a loop, whatever the journal says.
fn relaunch_if(moved: bool, me: &str, from: &str) -> Start {
    if moved && me != from {
        Start::Relaunch
    } else {
        Start::Normal
    }
}

/// `RollingBack` at a start: the files back, then, in the old version,
/// the store and the skip, and the journal goes.
fn rolling_back(sys: &dyn Sys, bin: &Bin, db: &Path, j: Journal) -> Start {
    let Ok(moved) = files_back(sys, bin, &j) else {
        // The program in place is whichever is there; the next start
        // tries again.
        return Start::Normal;
    };
    if sys.version() != j.from {
        // Not the old version: relaunch into it if this start just put it
        // back; otherwise leave the journal to the old one's next start.
        return relaunch_if(moved, &sys.version(), &j.from);
    }
    // A store the failed version migrated can't be opened by this one:
    // until it's restored, neither version starts and the journal stays.
    let mut j = j;
    if j.restore.is_none() {
        match choose(db, &j.snapshots_before) {
            Ok(chosen) => j.restore = Some(chosen),
            Err(_) => {
                log_event!(UPDATE_INSTALL_FAILED, error = "store_not_restored");
                return Start::Refuse;
            }
        }
        if write(bin, &j).is_err() {
            return Start::Refuse;
        }
    }
    if let Some(Some(snap)) = &j.restore
        && restore_store(sys, db, snap).is_err()
    {
        log_event!(UPDATE_INSTALL_FAILED, error = "store_not_restored");
        return Start::Refuse;
    }
    // A version that never started (the old one came back first) isn't
    // marked failed. `at_start` holds the lock (`skipped`'s one writer).
    let skipped = j.attempts == 0 || add_skip(bin, &j.to, SKIP_FAILED).is_ok();
    if !skipped {
        log_event!(UPDATE_INSTALL_FAILED, error = "skip_unsaved");
    }
    let _ = std::fs::remove_file(bin.path(FAILED));
    history::record_portable(
        bin,
        &j.from,
        &j.to,
        history::Outcome::RolledBack,
        j.attempt,
        &j.new_sha256,
        crate::unix_now(),
    );
    clear(bin);
    log_event!(UPDATE_INSTALL_FAILED, error = "rolled_back");
    Start::Normal
}

/// The installing run couldn't restart: the new program is taken out
/// again and the journal goes, so this run carries on as it was.
pub fn abandon(sys: &dyn Sys, data_dir: &Path) {
    let Ok(Some(bin)) = Bin::existing(data_dir) else {
        return;
    };
    let Ok(Some(j)) = read(&bin) else { return };
    if j.state == State::Swapped
        && j.from == sys.version()
        && j.attempts == 0
        && files_back(sys, &bin, &j).is_ok()
    {
        let _ = std::fs::remove_file(bin.path(FAILED));
        clear(&bin);
    }
}

/// Who has the last word on one probation: the self-test's verdict
/// ([`passed`] or [`failed`]) or a graceful stop's give-back ([`stopped`]),
/// never both, and never at once (each runs whole under one lock). Without
/// it a stop could read the journal at `Swapped`, `passed` remove
/// `previous.exe` and the journal, and the stop then write `Swapped` back
/// with no old program behind it (#659 review, N4).
#[derive(Debug, Clone, Default)]
pub struct Verdict(std::sync::Arc<std::sync::Mutex<Phase>>);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Open,
    Decided,
    Stopped,
}

impl Verdict {
    /// Runs the self-test's verdict `f` unless a stop came first. `None`
    /// then.
    pub fn decide<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let mut phase = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if *phase != Phase::Open {
            return None;
        }
        // Decided before it runs: a verdict that panics part way never
        // lets a stop give an attempt back over it.
        *phase = Phase::Decided;
        Some(f())
    }

    /// Runs the stop's give-back `f` unless the verdict came first.
    pub fn stop(&self, f: impl FnOnce()) -> bool {
        let mut phase = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if *phase != Phase::Open {
            return false;
        }
        *phase = Phase::Stopped;
        f();
        true
    }
}

/// The self-test passed: the update is kept.
pub fn passed(sys: &dyn Sys, data_dir: &Path) {
    let Ok(Some(bin)) = Bin::existing(data_dir) else {
        return;
    };
    let Ok(Some(j)) = read(&bin) else { return };
    if j.state != State::Swapped || j.to != sys.version() {
        return;
    }
    let _ = std::fs::remove_file(bin.path(PREVIOUS));
    history::record_portable(
        &bin,
        &j.from,
        &j.to,
        history::Outcome::Installed,
        j.attempt,
        &j.new_sha256,
        crate::unix_now(),
    );
    clear(&bin);
    log_event!(UPDATE_PROBATION_PASSED);
}

/// The self-test failed (`why` names the check) or the start did: the
/// files are swapped back. `true` when the caller should relaunch the old
/// version; `false` leaves it to the next start.
pub fn failed(sys: &dyn Sys, data_dir: &Path, why: &'static str) -> bool {
    let Ok(Some(bin)) = Bin::existing(data_dir) else {
        return false;
    };
    let Ok(Some(j)) = read(&bin) else {
        return false;
    };
    if j.state != State::Swapped || j.to != sys.version() {
        return false;
    }
    roll_back(sys, &bin, j, why) == Start::Relaunch
}

/// A graceful stop before the self-test passed (Ctrl+C) isn't a failure:
/// its attempt is given back.
pub fn stopped(sys: &dyn Sys, data_dir: &Path) {
    let Ok(Some(bin)) = Bin::existing(data_dir) else {
        return;
    };
    let Ok(Some(mut j)) = read(&bin) else { return };
    if j.state == State::Swapped && j.to == sys.version() {
        j.attempts = j.attempts.saturating_sub(1);
        let _ = write(&bin, &j);
    }
}

/// The time the self-test measures its window by ([`Tokio`] when
/// serving; a fake one in tests).
pub trait Clock {
    /// Time since the window opened.
    fn elapsed(&self) -> Duration;
    /// The pause between two checks.
    fn pause(&self) -> impl std::future::Future<Output = ()> + Send;
}

/// [`Clock`] from tokio's time, from when it was made.
#[derive(Debug, Clone, Copy)]
pub struct Tokio(pub tokio::time::Instant);

impl Clock for Tokio {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
    fn pause(&self) -> impl std::future::Future<Output = ()> + Send {
        tokio::time::sleep(Duration::from_millis(500))
    }
}

/// The self-test: `check` retried until it passes or `window` has passed
/// on `clock`. The failing check's name on error.
pub async fn self_test<F, Fut>(
    mut check: F,
    window: Duration,
    clock: &impl Clock,
) -> Result<(), &'static str>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), &'static str>>,
{
    loop {
        let last = match check().await {
            Ok(()) => return Ok(()),
            Err(what) => what,
        };
        if clock.elapsed() >= window {
            return Err(last);
        }
        clock.pause().await;
    }
}

/// The failed version's earliest pre-migration snapshot (one not there
/// before the install), with its SHA-256; `None` when it took none (the
/// store is kept: this version's own open refuses a newer schema). As the
/// MSI's rollback, by name alone.
fn choose(db: &Path, before: &[String]) -> io::Result<Option<Snapshot>> {
    let Some(s) = snapshots::list(db)?
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
    let (sha, _) = hash(&s.path)?;
    Ok(Some(Snapshot {
        name: s.name,
        sha256: sha.to_hex(),
    }))
}

/// The chosen snapshot renamed over the store, its side files removed
/// first. Done already when the snapshot is gone and the store is it; a
/// snapshot that changed since it was chosen, or that has its own journal
/// or WAL beside it (it was mid-write), is refused.
fn restore_store(sys: &dyn Sys, db: &Path, snap: &Snapshot) -> io::Result<()> {
    let refused = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_string());
    let stem = db
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("lodestar");
    if snap.name.contains(['/', '\\']) || snapshots::parse_name(stem, &snap.name).is_none() {
        return Err(refused("not a snapshot of this store"));
    }
    let path = db.with_file_name(&snap.name);
    if std::fs::symlink_metadata(&path).is_err() {
        return if is(db, &snap.sha256) {
            Ok(())
        } else {
            Err(refused("the snapshot is gone and the store isn't it"))
        };
    }
    for side in ["-wal", "-journal"] {
        if std::fs::symlink_metadata(db.with_file_name(format!("{}{side}", snap.name))).is_ok() {
            return Err(refused("the snapshot has a side file beside it"));
        }
    }
    if !is(&path, &snap.sha256) {
        return Err(refused("the snapshot changed since it was chosen"));
    }
    sys.fault(Point::Sides)?;
    let name = db.file_name().unwrap_or_default().to_string_lossy();
    for side in ["-wal", "-shm", "-journal"] {
        match std::fs::remove_file(db.with_file_name(format!("{name}{side}"))) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return Err(refused("the snapshot isn't a regular file"));
    }
    layout::rename_retrying(&path, db)?;
    sys.fault(Point::Restored)
}

/// Whether a portable rollback is under way in `data_dir`: the journal
/// at `rolling_back`. A journal that can't be read is an error (the
/// commands that open the store refuse then: `cli::util`); none, or no
/// `bin`, is `false`.
pub fn rollback_under_way(data_dir: &Path) -> io::Result<bool> {
    let Some(bin) = Bin::existing(data_dir)? else {
        return Ok(false);
    };
    Ok(read(&bin)?.is_some_and(|j| j.state == State::RollingBack))
}

fn read(bin: &Bin) -> io::Result<Option<Journal>> {
    let j: Option<Journal> = bin.read(JOURNAL)?;
    match j {
        Some(j) if j.format != FORMAT || !valid(&j) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "portable.json isn't one this version understands",
        )),
        j => Ok(j),
    }
}

fn valid(j: &Journal) -> bool {
    let sha = |s: &str| Sha256::from_hex(s).is_some();
    layout::parse_version(&j.from).is_some()
        && layout::parse_version(&j.to).is_some()
        && sha(&j.old_sha256)
        && sha(&j.new_sha256)
        && j.snapshots_before.len() <= 10_000
        && match &j.restore {
            Some(Some(snap)) => sha(&snap.sha256) && snap.name.len() <= 255,
            _ => true,
        }
}

fn write(bin: &Bin, j: &Journal) -> Result<(), &'static str> {
    bin.write(JOURNAL, j).map_err(|e| {
        log_event!(
            UPDATE_STATE_UNSAVED,
            file = JOURNAL,
            error = lodestar_log::Error::io(&e)
        );
        "journal_unsaved"
    })
}

fn clear(bin: &Bin) {
    let _ = bin.remove(JOURNAL);
}

/// Removes `dir` and what's in it, if it's there; a link in its place is
/// removed itself, never followed.
fn remove_tree(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(dir),
        Ok(_) => std::fs::remove_file(dir).or_else(|_| std::fs::remove_dir(dir)),
    }
}

fn hash(path: &Path) -> io::Result<(Sha256, u64)> {
    hash_from(&open_held(path)?)
}

/// Opens a file as itself (never through a link) for reading. On Windows
/// it's shared for reading and for a rename or delete, never for writing,
/// so what's read through it can't change while it's open.
fn open_held(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        const SHARE_READ: u32 = 0x1;
        const SHARE_DELETE: u32 = 0x4;
        const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ | SHARE_DELETE)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(path)?;
        let m = f.metadata()?;
        if m.file_attributes() & ATTRIBUTE_REPARSE_POINT != 0 || !m.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular file (a link isn't accepted)",
            ));
        }
        Ok(f)
    }
    #[cfg(not(windows))]
    {
        let f = layout::open_nofollow(path)?;
        if !f.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular file",
            ));
        }
        Ok(f)
    }
}

fn hash_from(mut f: &File) -> io::Result<(Sha256, u64)> {
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

/// [`Sys`] on Windows, as the user running the portable build.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

#[cfg(windows)]
impl Sys for System {
    /// As recorded at start (`windows::relaunch::program`), so a rename
    /// of the running file doesn't change it.
    fn program(&self) -> PathBuf {
        crate::windows::relaunch::program().unwrap_or_default()
    }

    fn version(&self) -> String {
        env!("LODESTAR_BUILD_VERSION").to_string()
    }

    /// `windows::acl::log_folder`'s rule, for the account this runs as.
    /// Its PowerShell is started without the relaunch token.
    /// A folder found private stays so for the run (only this user,
    /// SYSTEM or Administrators could change that), so `/updates` doesn't
    /// start PowerShell on every poll; one that isn't is checked again.
    fn bin_private(&self, dir: &Path) -> Result<(), String> {
        static PRIVATE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
        let mut seen = PRIVATE.lock().unwrap_or_else(|e| e.into_inner());
        if seen.as_deref() == Some(dir) {
            return Ok(());
        }
        crate::windows::acl::log_folder(dir)?;
        *seen = Some(dir.to_path_buf());
        Ok(())
    }

    fn restart(&self) -> io::Result<()> {
        let r = crate::system::Restart::global();
        if r.info().method != lodestar_api::system::RestartMethod::Relaunch {
            return Err(io::Error::other("this run can't relaunch itself"));
        }
        // Starts the new copy from `lodestar.exe` now, so a failure is
        // known before this run stops.
        r.prepare()?;
        r.request();
        Ok(())
    }

    /// With the protected DACL (this user, SYSTEM, Administrators) in one
    /// step, through `windows::acl` (no relaunch token in its
    /// environment). Without this user's SID it would lock the user out:
    /// refused.
    fn create_bin(&self, dir: &Path) -> io::Result<()> {
        use crate::windows::acl::{self, Ops};
        let own = acl::process_sid()
            .ok_or_else(|| io::Error::other("this account's SID couldn't be read"))?;
        acl::Windows { own_sid: Some(own) }.create(dir)
    }
}

/// For [`Start::Relaunch`] at a start, before anything is served: the
/// program at `lodestar.exe` as a new copy (PK-win-restart's command, the
/// same arguments), handed over at once, since this process holds no port
/// or store yet. The caller then exits with `EXIT_RELAUNCHED`.
#[cfg(windows)]
pub fn relaunch_now(program: &Path) -> io::Result<()> {
    use crate::windows::relaunch;
    use std::io::Write;
    let token = relaunch::token();
    let mut child = relaunch::command(program, std::env::args_os().skip(1), &token).spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no pipe to the new copy"))?;
    stdin.write_all(format!("{token}\n").as_bytes())?;
    stdin.flush()?;
    // End of file: the new copy needn't wait for this process to exit.
    drop(stdin);
    Ok(())
}

/// What the self-test probes (`Services::portable_probe`): as the Unix
/// child's (§12.2), the store open with its migrations done (it is, once
/// the services have started), the search index answering, the HTTP port
/// bound (and the DHT's unless it's off), and `/healthz` green.
#[derive(Clone)]
pub struct Probe {
    pub http: std::net::SocketAddr,
    /// `None` with the DHT off.
    pub dht: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    pub search: Option<lodestar_search::SearchHandle>,
}

/// One pass of the self-test's checks; the failing one's name.
pub async fn check_once(probe: &Probe) -> Result<(), &'static str> {
    if let Some(search) = probe.search.clone() {
        // A blocking read of the index: off the async runtime.
        tokio::task::spawn_blocking(move || {
            let index = search.index();
            let node = lodestar_search::query::parse("lodestar").map_err(|_| "the search index")?;
            let compiled = index.query(&node, crate::unix_now());
            index
                .search(&compiled, 1)
                .map(drop)
                .map_err(|_| "the search index")
        })
        .await
        .map_err(|_| "the search index")??;
    }
    if probe.dht.as_ref().is_some_and(|bound| !bound()) {
        return Err("the DHT port");
    }
    let addr = probe.http;
    tokio::task::spawn_blocking(move || crate::healthcheck(addr))
        .await
        .map_err(|_| "/healthz")?
        .map_err(|_| "/healthz")
}

/// `main`'s `serve` on Windows: the steps above at their places.
#[cfg(windows)]
pub mod serve {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use super::{Lock, Start, Sys, System};
    use crate::update::windows::check::{self, InstallKind};

    /// After logging starts and before anything opens the store:
    /// `Ok(Some(lock))` when this is the new version on probation, holding
    /// `<data>\bin\.lock` until the self-test decides ([`after_start`]);
    /// `Err` to exit with, after relaunching the program now in place.
    pub fn before(config: &crate::config::Config) -> Result<Option<Lock>, ExitCode> {
        let data = &config.data_dir;
        if check::detect(&check::signals(data)) != InstallKind::Portable {
            return Ok(None);
        }
        if let Err(e) = super::ensure_bin(&System, data) {
            eprintln!("lodestar serve: the update folder couldn't be created privately: {e}");
        }
        match super::begin(&System, data, &config.db_path()) {
            (Start::Normal, _) => Ok(None),
            (Start::Probation, lock) => Ok(lock),
            (Start::Relaunch, _) => Err(relaunch().map_or(ExitCode::FAILURE, ExitCode::from)),
            (Start::Refuse, _) => {
                eprintln!(
                    "lodestar serve: an update's rollback couldn't restore the store; the \
                     snapshot is still in {}, and the reason is in the log. Neither version \
                     starts until it's restored.",
                    data.display()
                );
                Err(ExitCode::FAILURE)
            }
        }
    }

    /// Starts the program in place as a new copy: `Some(EXIT_RELAUNCHED)`
    /// once it's started. (No loop: see `relaunch_if`.)
    fn relaunch() -> Option<u8> {
        match super::relaunch_now(&System.program()) {
            Ok(()) => Some(crate::system::restart::EXIT_RELAUNCHED as u8),
            Err(e) => {
                eprintln!("lodestar serve: starting the program in place failed: {e}");
                None
            }
        }
    }

    /// After `Services::start`, on probation: the self-test in the
    /// background. Passed, the update is kept; failed, the old program is
    /// put back and this run restarts into it (Restart now's relaunch,
    /// from `lodestar.exe`).
    pub fn after_start(data: PathBuf, probe: super::Probe, lock: Lock) -> super::Verdict {
        let verdict = super::Verdict::default();
        let decide = verdict.clone();
        tokio::spawn(async move {
            let clock = super::Tokio(tokio::time::Instant::now());
            let check = || {
                let p = probe.clone();
                async move { super::check_once(&p).await }
            };
            let result = super::self_test(check, super::PROBATION, &clock).await;
            let back = tokio::task::spawn_blocking(move || {
                decide.decide(|| match result {
                    Ok(()) => {
                        super::passed(&System, &data);
                        false
                    }
                    Err(why) => super::failed(&System, &data, why),
                })
            })
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
            // Decided: the notifier's next try takes the lock. Released
            // before a relaunch, so the old version's start can take it.
            drop(lock);
            // Restart now's relaunch, from `lodestar.exe` (the old program
            // again); if it can't start, this run keeps serving and says so.
            if back && System.restart().is_err() {
                lodestar_log::log_event!(
                    crate::update::log_events::UPDATE_INSTALL_FAILED,
                    error = "restart_unavailable"
                );
            }
        });
        verdict
    }

    /// `Services::start` failed on probation: the old program goes back
    /// and starts now. `Some` exit code then.
    pub fn start_failed(data: &Path, lock: Lock) -> Option<u8> {
        let back = super::failed(&System, data, "start");
        // Released before the relaunch: the old version's start takes it.
        drop(lock);
        if !back {
            return None;
        }
        relaunch()
    }

    /// The run ended before the self-test decided: a graceful stop.
    /// Off the async runtime: the stop may wait for a verdict (a
    /// rollback) running on the blocking pool.
    pub async fn stopped(data: PathBuf, verdict: super::Verdict) {
        let _ = tokio::task::spawn_blocking(move || {
            verdict.stop(|| super::stopped(&System, &data));
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use lodestar_update::semver::Version;

    use super::*;

    const OLD: &[u8] = b"old program 0.5.4";
    const NEW: &[u8] = b"new program 0.5.5";

    /// A folder unpacked from the zip: `app\lodestar.exe`, `app\data`.
    struct Fake {
        _tmp: tempfile::TempDir,
        app: PathBuf,
        version: RefCell<String>,
        private: Cell<bool>,
        fault: Cell<Option<Point>>,
        /// Rewrites a file in `bin` (`next.exe` unless set) at this point
        /// instead of failing.
        tamper: Cell<Option<Point>>,
        tamper_file: Cell<&'static str>,
        /// Whether that rewrite got through (Windows refuses it while the
        /// file is held).
        tampered: Cell<bool>,
        /// The points reached, in order.
        seen: RefCell<Vec<Point>>,
    }

    impl Fake {
        fn new() -> Fake {
            let tmp = tempfile::tempdir().unwrap();
            let app = tmp.path().join("app");
            std::fs::create_dir_all(app.join("data").join("bin")).unwrap();
            std::fs::write(app.join(PROGRAM), OLD).unwrap();
            Fake {
                _tmp: tmp,
                app,
                version: RefCell::new("0.5.4".into()),
                private: Cell::new(true),
                fault: Cell::new(None),
                tamper: Cell::new(None),
                tamper_file: Cell::new(NEXT),
                tampered: Cell::new(false),
                seen: RefCell::new(Vec::new()),
            }
        }
        fn data(&self) -> PathBuf {
            self.app.join("data")
        }
        fn db(&self) -> PathBuf {
            self.data().join("lodestar.db")
        }
        fn bin(&self) -> Bin {
            Bin::open(&self.data()).unwrap()
        }
        fn at(&self, name: &str) -> Option<Vec<u8>> {
            std::fs::read(self.data().join("bin").join(name)).ok()
        }
        fn program_bytes(&self) -> Option<Vec<u8>> {
            std::fs::read(self.app.join(PROGRAM)).ok()
        }
        fn journal(&self) -> Option<Journal> {
            read(&self.bin()).unwrap()
        }
        fn run_as(&self, v: &str) {
            *self.version.borrow_mut() = v.into();
        }
        /// The zip and its pin, downloaded to `bin`.
        fn download(&self, version: &str, entry: &str, body: &[u8]) -> (PathBuf, Pin) {
            let bytes = zip(&[(entry, body), ("README.txt", b"read me")]);
            let part = self.data().join("bin").join(format!("{version}.part"));
            std::fs::write(&part, &bytes).unwrap();
            let pin = Pin {
                version: Version::parse(version).unwrap(),
                url: "https://example.invalid/l.zip".into(),
                sha256: Sha256::of(&bytes),
                size: bytes.len() as u64,
            };
            (part, pin)
        }
        fn release(&self) -> (PathBuf, Pin) {
            self.download(
                "0.5.5",
                "lodestar-0.5.5-x86_64-windows-portable/lodestar.exe",
                NEW,
            )
        }
        fn install(&self) -> Result<(), &'static str> {
            let (part, pin) = self.release();
            install(self, &self.data(), &self.db(), &self.bin(), &part, &pin)
        }
        /// Nothing changed: the old program in place, nothing staged.
        fn untouched(&self) {
            assert_eq!(self.program_bytes().as_deref(), Some(OLD));
            for name in [JOURNAL, NEXT, PREVIOUS, STAGING] {
                assert!(self.at(name).is_none(), "{name} left behind");
            }
            assert!(!self.data().join("bin").join(STAGING).exists());
        }
    }

    impl Sys for Fake {
        fn program(&self) -> PathBuf {
            self.app.join(PROGRAM)
        }
        fn version(&self) -> String {
            self.version.borrow().clone()
        }
        fn bin_private(&self, _dir: &Path) -> Result<(), String> {
            if self.private.get() {
                Ok(())
            } else {
                Err("shared".into())
            }
        }
        fn create_bin(&self, dir: &Path) -> io::Result<()> {
            std::fs::create_dir(dir)
        }
        fn restart(&self) -> io::Result<()> {
            Ok(())
        }
        fn fault(&self, point: Point) -> io::Result<()> {
            self.seen.borrow_mut().push(point);
            if self.tamper.get() == Some(point) {
                let wrote = std::fs::write(
                    self.data().join("bin").join(self.tamper_file.get()),
                    b"swapped in",
                );
                self.tampered.set(wrote.is_ok());
            }
            if self.fault.get() == Some(point) {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
    }

    /// A zip of stored entries, as `Compress-Archive` would hold them.
    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, body) in entries {
            let mut crc = flate2::Crc::new();
            crc.update(body);
            let (crc, len, offset) = (crc.sum(), body.len() as u32, out.len() as u32);
            let common = |v: &mut Vec<u8>| {
                v.extend_from_slice(&20u16.to_le_bytes()); // version needed
                v.extend_from_slice(&0u16.to_le_bytes()); // flags
                v.extend_from_slice(&0u16.to_le_bytes()); // stored
                v.extend_from_slice(&0u32.to_le_bytes()); // time and date
                v.extend_from_slice(&crc.to_le_bytes());
                v.extend_from_slice(&len.to_le_bytes());
                v.extend_from_slice(&len.to_le_bytes());
                v.extend_from_slice(&(name.len() as u16).to_le_bytes());
                v.extend_from_slice(&0u16.to_le_bytes()); // extra
            };
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            common(&mut out);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(body);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes()); // made by: DOS
            common(&mut central);
            central.extend_from_slice(&[0; 6]); // comment, disk, internal
            central.extend_from_slice(&0u32.to_le_bytes()); // external
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let (at, size) = (out.len() as u32, central.len() as u32);
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        let n = entries.len() as u16;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn installs_the_new_program_and_keeps_the_old_one_aside() {
        let f = Fake::new();
        assert_eq!(f.install(), Ok(()));
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        assert_eq!(f.at(PREVIOUS).as_deref(), Some(OLD));
        assert!(f.at(NEXT).is_none());
        assert!(f.at("0.5.5.part").is_none());
        let j = f.journal().unwrap();
        assert_eq!((j.state, j.attempts), (State::Swapped, 0));
        assert_eq!((j.from.as_str(), j.to.as_str()), ("0.5.4", "0.5.5"));
        assert_eq!(j.old_sha256, Sha256::of(OLD).to_hex());
        assert_eq!(j.new_sha256, Sha256::of(NEW).to_hex());
    }

    #[test]
    fn a_download_that_doesnt_match_its_pin_is_never_unpacked() {
        let f = Fake::new();
        let (part, mut pin) = f.release();
        pin.size += 1;
        assert_eq!(
            install(&f, &f.data(), &f.db(), &f.bin(), &part, &pin),
            Err("verify_failed")
        );
        f.untouched();
        let (part, mut pin) = f.release();
        pin.sha256 = Sha256::of(b"something else");
        assert_eq!(
            install(&f, &f.data(), &f.db(), &f.bin(), &part, &pin),
            Err("verify_failed")
        );
        f.untouched();
    }

    #[test]
    fn a_zip_without_this_versions_program_is_refused() {
        let f = Fake::new();
        for entry in [
            "lodestar-0.5.6-x86_64-windows-portable/lodestar.exe",
            "lodestar.exe",
            "lodestar-0.5.5-x86_64-windows-portable/../lodestar.exe",
        ] {
            let (part, pin) = f.download("0.5.5", entry, NEW);
            assert_eq!(
                install(&f, &f.data(), &f.db(), &f.bin(), &part, &pin),
                Err("zip_refused"),
                "{entry}"
            );
            f.untouched();
        }
    }

    #[test]
    fn every_failure_while_installing_leaves_the_running_program_in_place() {
        for point in [
            Point::Extracted,
            Point::Aside,
            Point::Between,
            Point::Recorded,
        ] {
            let f = Fake::new();
            f.fault.set(Some(point));
            assert!(f.install().is_err(), "{point:?}");
            f.untouched();
            // And the next try works.
            f.fault.set(None);
            assert_eq!(f.install(), Ok(()), "{point:?}");
        }
    }

    #[test]
    fn a_staged_program_changed_before_its_move_is_never_put_in_place() {
        let f = Fake::new();
        f.tamper.set(Some(Point::Aside));
        assert_eq!(f.install(), Err("swap_failed"));
        assert!(f.tampered.get());
        f.untouched();
        // Refused before the running program was moved at all.
        assert!(!f.seen.borrow().contains(&Point::Between));
    }

    #[test]
    fn an_attempt_that_cant_be_counted_rolls_back() {
        let f = Fake::new();
        f.install().unwrap();
        // The journal can be read but not written.
        std::fs::create_dir(f.data().join("bin").join(".portable.json.tmp")).unwrap();
        f.run_as("0.5.5");
        assert_ne!(at_start(&f, &f.data(), &f.db()), Start::Probation);
    }

    #[test]
    fn refuses_a_shared_bin_another_folder_or_an_update_under_way() {
        let f = Fake::new();
        f.private.set(false);
        assert_eq!(f.install(), Err("bin_not_private"));
        f.untouched();
        f.private.set(true);
        // A data folder that isn't the program's own.
        let (part, pin) = f.release();
        let other = f.app.join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(
            install(&f, &other, &f.db(), &f.bin(), &part, &pin),
            Err("install_kind_unknown")
        );
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
        assert_eq!(f.install(), Ok(()));
        assert_eq!(
            cannot_install(&f, &f.data(), &f.bin()),
            Some("update_in_progress")
        );
    }

    #[test]
    fn the_new_version_gets_three_starts_then_the_old_one_comes_back() {
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        for n in 1..=MAX_ATTEMPTS {
            assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
            assert_eq!(f.journal().unwrap().attempts, n);
        }
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Relaunch);
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
        assert_eq!(f.at(FAILED).as_deref(), Some(NEW));
        assert_eq!(f.journal().unwrap().state, State::RollingBack);
        // The old version finishes it.
        f.run_as("0.5.4");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert!(f.journal().is_none());
        assert!(f.at(FAILED).is_none() && f.at(PREVIOUS).is_none());
        let skipped = std::fs::read_to_string(f.data().join("bin").join("skipped")).unwrap();
        assert!(
            skipped.contains("\"0.5.5\"") && skipped.contains("failed"),
            "{skipped}"
        );
        // Recorded once the rollback finishes, in the old version.
        assert_eq!(
            history_of(&f),
            [("0.5.5".into(), history::Outcome::RolledBack)]
        );
    }

    #[test]
    fn a_run_that_cant_restart_puts_its_own_program_back() {
        let f = Fake::new();
        f.install().unwrap();
        // Only the installing version, before the new one ever started.
        f.run_as("0.5.5");
        abandon(&f, &f.data());
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        f.run_as("0.5.4");
        abandon(&f, &f.data());
        f.untouched();
        assert!(f.at(FAILED).is_none());
    }

    #[test]
    fn a_graceful_stop_on_probation_gives_its_attempt_back() {
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        stopped(&f, &f.data());
        assert_eq!(f.journal().unwrap().attempts, 0);
        // Only the new version on probation counts.
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        f.run_as("0.5.4");
        stopped(&f, &f.data());
        assert_eq!(f.journal().unwrap().attempts, 1);
        // Nor once it's rolling back: the count says it ran.
        f.run_as("0.5.5");
        assert!(failed(&f, &f.data(), "/healthz"));
        stopped(&f, &f.data());
        assert_eq!(f.journal().unwrap().attempts, 1);
    }

    #[test]
    fn a_passed_self_test_keeps_the_update() {
        let f = Fake::new();
        f.install().unwrap();
        // Each install attempt has its own id, for its history entry.
        assert!(f.journal().unwrap().attempt.is_some());
        // Only the new version can pass it.
        passed(&f, &f.data());
        assert!(f.journal().is_some());
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        passed(&f, &f.data());
        assert!(f.journal().is_none() && f.at(PREVIOUS).is_none());
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        // One history entry, and none for the repeat.
        passed(&f, &f.data());
        assert_eq!(
            history_of(&f),
            [("0.5.5".into(), history::Outcome::Installed)]
        );
    }

    fn history_of(f: &Fake) -> Vec<(String, history::Outcome)> {
        let bin = Bin::open(&f.data()).unwrap();
        history::read(&bin)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| (e.version, e.result))
            .collect()
    }

    #[test]
    fn a_failed_self_test_swaps_the_programs_back_and_relaunches() {
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        assert!(failed(&f, &f.data(), "/healthz"));
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
        assert_eq!(f.journal().unwrap().state, State::RollingBack);
        // Not twice.
        assert!(!failed(&f, &f.data(), "/healthz"));
        // And never for the old version while the new one is in place.
        let f = Fake::new();
        f.install().unwrap();
        assert!(!failed(&f, &f.data(), "start"));
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        assert_eq!(f.journal().unwrap().state, State::Swapped);
    }

    #[test]
    fn a_rollback_that_cant_move_the_files_is_finished_by_the_next_start() {
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        f.fault.set(Some(Point::Back));
        assert!(!failed(&f, &f.data(), "/healthz"));
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        assert_eq!(f.journal().unwrap().state, State::RollingBack);
        f.fault.set(None);
        // The new version starts again and puts the old one back.
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Relaunch);
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
    }

    #[test]
    fn the_old_version_restores_the_store_the_new_one_migrated() {
        let f = Fake::new();
        let before = f.data().join("lodestar.pre-v3.db");
        std::fs::write(&before, b"older snapshot").unwrap();
        f.install().unwrap();
        f.run_as("0.5.5");
        at_start(&f, &f.data(), &f.db());
        // The new version migrates: a snapshot, a newer store, a WAL.
        std::fs::write(f.data().join("lodestar.pre-v5.db"), b"store at v4").unwrap();
        std::fs::write(f.data().join("lodestar.pre-v6.db"), b"store at v5").unwrap();
        std::fs::write(f.db(), b"store at v6").unwrap();
        std::fs::write(f.data().join("lodestar.db-wal"), b"wal").unwrap();
        assert!(failed(&f, &f.data(), "/healthz"));
        f.run_as("0.5.4");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store at v4");
        assert!(!f.data().join("lodestar.db-wal").exists());
        assert!(
            before.exists(),
            "a snapshot from before the install is kept"
        );
    }

    /// The failed version migrated twice: `pre-v5` (a v4 store) is the one
    /// to restore, `pre-v6` must never be.
    fn migrated_twice() -> Fake {
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        at_start(&f, &f.data(), &f.db());
        std::fs::write(f.data().join("lodestar.pre-v5.db"), b"store at v4").unwrap();
        std::fs::write(f.data().join("lodestar.pre-v6.db"), b"store at v5").unwrap();
        std::fs::write(f.db(), b"store at v6").unwrap();
        std::fs::write(f.data().join("lodestar.db-wal"), b"wal").unwrap();
        assert!(failed(&f, &f.data(), "/healthz"));
        f.run_as("0.5.4");
        f
    }

    #[test]
    fn a_crash_during_the_restore_never_restores_another_snapshot() {
        // After the rename, before the journal goes.
        let f = migrated_twice();
        f.fault.set(Some(Point::Restored));
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Refuse);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store at v4");
        f.fault.set(None);
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store at v4");
        assert!(f.journal().is_none());
        // After the side files go, before the rename.
        let f = migrated_twice();
        f.fault.set(Some(Point::Sides));
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Refuse);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store at v6");
        f.fault.set(None);
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store at v4");
    }

    #[test]
    fn a_restore_that_cant_be_done_safely_starts_neither_version() {
        // The chosen snapshot changed afterwards.
        let f = migrated_twice();
        f.fault.set(Some(Point::Sides));
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Refuse);
        f.fault.set(None);
        std::fs::write(f.data().join("lodestar.pre-v5.db"), b"swapped").unwrap();
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Refuse);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store at v6");
        assert_eq!(f.journal().unwrap().state, State::RollingBack);
        // A snapshot with its own WAL beside it was mid-write.
        let f = migrated_twice();
        std::fs::write(f.data().join("lodestar.pre-v5.db-wal"), b"").unwrap();
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Refuse);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store at v6");
        // Gone, and the store isn't it.
        let f = migrated_twice();
        f.fault.set(Some(Point::Sides));
        at_start(&f, &f.data(), &f.db());
        f.fault.set(None);
        std::fs::remove_file(f.data().join("lodestar.pre-v5.db")).unwrap();
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Refuse);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store at v6");
    }

    #[test]
    fn a_journal_in_a_shared_bin_is_never_acted_on() {
        let f = Fake::new();
        crashed(&f, State::Swapped, Some(NEW), Some(OLD));
        f.private.set(false);
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        assert_eq!(f.at(PREVIOUS).as_deref(), Some(OLD));
        assert_eq!(f.journal().unwrap().attempts, 0);
    }

    #[test]
    fn when_the_old_program_cant_go_back_the_new_one_stays() {
        // Installing: the new program can't move in, nor the old one back.
        let f = Fake::new();
        // Between fails the new program's move into place; PutBack then
        // fails the undo that would put the old one back.
        struct Both<'a>(&'a Fake);
        impl Sys for Both<'_> {
            fn program(&self) -> PathBuf {
                self.0.program()
            }
            fn version(&self) -> String {
                self.0.version()
            }
            fn bin_private(&self, d: &Path) -> Result<(), String> {
                self.0.bin_private(d)
            }
            fn create_bin(&self, d: &Path) -> io::Result<()> {
                self.0.create_bin(d)
            }
            fn restart(&self) -> io::Result<()> {
                Ok(())
            }
            fn fault(&self, p: Point) -> io::Result<()> {
                if matches!(p, Point::Between | Point::PutBack) {
                    Err(io::Error::other("injected"))
                } else {
                    Ok(())
                }
            }
        }
        let (part, pin) = f.release();
        assert!(install(&Both(&f), &f.data(), &f.db(), &f.bin(), &part, &pin).is_err());
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        assert!(f.journal().is_some());
        // The next start makes it the new version's probation.
        f.fault.set(None);
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        // Rolling back: the old program can't go back, so the new one does.
        f.fault.set(Some(Point::PutBack));
        assert!(!failed(&f, &f.data(), "/healthz"));
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        f.fault.set(None);
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Relaunch);
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
    }

    /// Between the check and the move: on Windows the held handle
    /// refuses the write, so the checked program is the one moved; where
    /// the write gets through, the check after the move takes it out again.
    #[test]
    fn a_program_swapped_in_after_the_check_is_taken_out_again() {
        let f = Fake::new();
        f.tamper.set(Some(Point::Between));
        let result = f.install();
        if cfg!(windows) {
            assert!(!f.tampered.get(), "the held handle let a writer in");
            assert_eq!(result, Ok(()));
            assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        } else {
            assert!(f.tampered.get(), "the write should have got through here");
            assert_eq!(result, Err("swap_failed"));
            f.untouched();
        }
    }

    #[test]
    fn a_store_the_new_version_never_migrated_is_kept() {
        let f = Fake::new();
        std::fs::write(f.db(), b"store").unwrap();
        std::fs::write(f.data().join("lodestar.pre-v3.db"), b"older").unwrap();
        f.install().unwrap();
        f.run_as("0.5.5");
        at_start(&f, &f.data(), &f.db());
        assert!(failed(&f, &f.data(), "/healthz"));
        f.run_as("0.5.4");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(std::fs::read(f.db()).unwrap(), b"store");
    }

    /// The journal as a crash left it, with the files to match.
    fn crashed(f: &Fake, state: State, program: Option<&[u8]>, previous: Option<&[u8]>) {
        let j = Journal {
            format: FORMAT,
            from: "0.5.4".into(),
            to: "0.5.5".into(),
            old_sha256: Sha256::of(OLD).to_hex(),
            new_sha256: Sha256::of(NEW).to_hex(),
            snapshots_before: vec![],
            attempts: 0,
            restore: None,
            state,
            attempt: None,
        };
        f.bin().write(JOURNAL, &j).unwrap();
        let path = f.app.join(PROGRAM);
        match program {
            Some(b) => std::fs::write(&path, b).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        if let Some(b) = previous {
            std::fs::write(f.data().join("bin").join(PREVIOUS), b).unwrap();
        }
    }

    #[test]
    fn a_crash_at_any_step_is_finished_at_the_next_start() {
        // Staged: the staged program goes.
        let f = Fake::new();
        crashed(&f, State::Staged, Some(OLD), None);
        std::fs::write(f.data().join("bin").join(NEXT), NEW).unwrap();
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        f.untouched();
        // Swapping, before the rename aside.
        let f = Fake::new();
        crashed(&f, State::Swapping, Some(OLD), None);
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        f.untouched();
        // Swapping, interrupted between the renames and started some way
        // other than the launcher: the old program is put back.
        let f = Fake::new();
        crashed(&f, State::Swapping, None, Some(OLD));
        std::fs::write(f.data().join("bin").join(NEXT), NEW).unwrap();
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        f.untouched();
        // Swapping, interrupted between the renames, after the launcher
        // moved `previous.exe` back: the old program in place, `next.exe`
        // still staged.
        let f = Fake::new();
        crashed(&f, State::Swapping, Some(OLD), None);
        std::fs::write(f.data().join("bin").join(NEXT), NEW).unwrap();
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        f.untouched();
        // Swapping, both renames done: the new version's probation.
        let f = Fake::new();
        crashed(&f, State::Swapping, Some(NEW), Some(OLD));
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        assert_eq!(f.journal().unwrap().state, State::Swapped);
        // ...unless the old version is the one starting: it's rolled back,
        // and the version, which never ran, isn't marked failed.
        let f = Fake::new();
        crashed(&f, State::Swapping, Some(NEW), Some(OLD));
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
        assert!(f.journal().is_none());
        assert!(f.at("skipped").is_none());
    }

    #[test]
    fn a_journal_that_doesnt_match_the_program_isnt_trusted() {
        // `Swapped`, but what's in place isn't the new program.
        let f = Fake::new();
        crashed(&f, State::Swapped, Some(b"someone else's"), Some(OLD));
        f.run_as("0.5.5");
        assert_ne!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        assert_eq!(f.program_bytes().as_deref(), Some(&b"someone else's"[..]));
        // A previous program that isn't the recorded one is never put back.
        let f = Fake::new();
        crashed(&f, State::RollingBack, None, Some(b"planted"));
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert!(f.program_bytes().is_none());
        assert_eq!(f.journal().unwrap().state, State::RollingBack);
    }

    #[test]
    fn a_damaged_journal_is_left_and_installs_wait() {
        let f = Fake::new();
        std::fs::write(f.data().join("bin").join(JOURNAL), b"{not json").unwrap();
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(f.install(), Err("update_in_progress"));
        let mut j = serde_json::to_value(Journal {
            format: FORMAT,
            from: "0.5.4".into(),
            to: "0.5.5".into(),
            old_sha256: "nope".into(),
            new_sha256: Sha256::of(NEW).to_hex(),
            snapshots_before: vec![],
            attempts: 0,
            restore: None,
            state: State::Swapped,
            attempt: None,
        })
        .unwrap();
        f.bin().write(JOURNAL, &j).unwrap();
        assert!(read(&f.bin()).is_err());
        j["old_sha256"] = Sha256::of(OLD).to_hex().into();
        j["format"] = 2.into();
        f.bin().write(JOURNAL, &j).unwrap();
        assert!(read(&f.bin()).is_err());
        // Unknown fields from a newer version are fine.
        j["format"] = FORMAT.into();
        j["later"] = true.into();
        f.bin().write(JOURNAL, &j).unwrap();
        assert!(read(&f.bin()).unwrap().is_some());
    }

    /// Hostile or corrupt journals: each is refused and leaves the
    /// program in place, untouched.
    #[test]
    fn a_hostile_journal_never_moves_the_program() {
        let good = || {
            serde_json::to_value(Journal {
                format: FORMAT,
                from: "0.5.4".into(),
                to: "0.5.5".into(),
                old_sha256: Sha256::of(OLD).to_hex(),
                new_sha256: Sha256::of(NEW).to_hex(),
                snapshots_before: vec![],
                attempts: 0,
                restore: None,
                state: State::Swapped,
                attempt: None,
            })
            .unwrap()
        };
        let mut shapes: Vec<serde_json::Value> = Vec::new();
        for (key, value) in [
            ("format", serde_json::json!(0)),
            ("from", serde_json::json!("not a version")),
            ("to", serde_json::json!("../../x")),
            ("new_sha256", serde_json::json!("00")),
            ("state", serde_json::json!("deleting")),
            ("attempts", serde_json::json!(-1)),
            ("snapshots_before", serde_json::json!(vec!["x"; 10_001])),
        ] {
            let mut j = good();
            j[key] = value;
            shapes.push(j);
        }
        let mut missing = good();
        missing.as_object_mut().unwrap().remove("state");
        shapes.push(missing);
        for j in shapes {
            let f = Fake::new();
            std::fs::write(f.data().join("bin").join(PREVIOUS), OLD).unwrap();
            f.bin().write(JOURNAL, &j).ok();
            f.run_as("0.5.5");
            assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal, "{j}");
            assert_eq!(f.program_bytes().as_deref(), Some(OLD), "{j}");
            assert!(f.at(PREVIOUS).is_some(), "{j}");
        }
        // Oversize.
        let f = Fake::new();
        let big = vec![b' '; layout::MAX_STATE as usize + 1];
        std::fs::write(f.data().join("bin").join(JOURNAL), big).unwrap();
        assert!(read(&f.bin()).is_err());
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
        // A count at its limit can't overflow: it rolls back.
        let f = Fake::new();
        crashed(&f, State::Swapped, Some(NEW), Some(OLD));
        let mut j = f.journal().unwrap();
        j.attempts = u32::MAX;
        f.bin().write(JOURNAL, &j).unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Relaunch);
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
    }

    #[test]
    fn two_programs_never_hand_over_in_a_loop() {
        // A program whose version is neither the journal's `from` nor its
        // `to` (a mis-versioned zip): it moves itself out and relaunches
        // once; the old program then finishes without relaunching.
        let f = Fake::new();
        crashed(&f, State::Swapped, Some(NEW), Some(OLD));
        f.run_as("0.9.9");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Relaunch);
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
        f.run_as("0.5.4");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert!(f.journal().is_none());
        // A journal whose `from` isn't the old program's version: nothing
        // to move, so no relaunch, however often it starts.
        let f = Fake::new();
        crashed(&f, State::RollingBack, Some(OLD), None);
        f.run_as("0.9.9");
        for _ in 0..3 {
            assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        }
        assert_eq!(f.program_bytes().as_deref(), Some(OLD));
    }

    /// A rollback checks the old program before it moves the new one
    /// out: with a `previous.exe` that isn't the recorded one, the new
    /// program stays where it is.
    #[test]
    fn a_rollback_never_moves_the_new_program_out_for_a_wrong_previous() {
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        std::fs::write(f.data().join("bin").join(PREVIOUS), b"planted").unwrap();
        f.seen.borrow_mut().clear();
        assert!(!failed(&f, &f.data(), "/healthz"));
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        assert!(f.at(FAILED).is_none());
        // Refused before the new program was moved out at all.
        assert!(!f.seen.borrow().contains(&Point::PutBack));
    }

    /// After the old program is moved back, it's checked again: one that
    /// changed under the move never stays at `lodestar.exe`. On Windows
    /// the held handle refuses the change itself.
    #[test]
    fn an_old_program_changed_during_its_move_back_never_stays_in_place() {
        // Put back at a start (a swap interrupted between its renames,
        // started some way other than the launcher).
        let f = Fake::new();
        crashed(&f, State::Swapping, None, Some(OLD));
        f.tamper_file.set(PREVIOUS);
        f.tamper.set(Some(Point::PutBack));
        at_start(&f, &f.data(), &f.db());
        assert_ne!(f.program_bytes().as_deref(), Some(&b"swapped in"[..]));
        assert_eq!(f.tampered.get(), !cfg!(windows));
        // Rolled back from the new version.
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        f.tamper_file.set(PREVIOUS);
        f.tamper.set(Some(Point::PutBack));
        let back = failed(&f, &f.data(), "/healthz");
        assert_eq!(f.tampered.get(), !cfg!(windows));
        assert_ne!(f.program_bytes().as_deref(), Some(&b"swapped in"[..]));
        if !cfg!(windows) {
            // The new program stays rather than an unchecked one.
            assert!(!back);
            assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        }
    }

    /// The lock is held from the probation copy's start, not only once
    /// its notifier runs, so a second copy started while the first is
    /// still starting judges nothing.
    #[test]
    fn the_probation_copy_holds_the_lock_from_its_start() {
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        let (start, lock) = begin(&f, &f.data(), &f.db());
        assert_eq!(start, Start::Probation);
        assert!(lock.is_some());
        // A second copy while the first is still starting.
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        assert_eq!(f.journal().unwrap().attempts, 1);
        drop(lock);
        assert_eq!(begin(&f, &f.data(), &f.db()).0, Start::Probation);
        assert_eq!(f.journal().unwrap().attempts, 2);
        // Only probation keeps it.
        f.run_as("0.5.4");
        let (start, lock) = begin(&f, &f.data(), &f.db());
        assert_ne!(start, Start::Probation);
        assert!(lock.is_none());
    }

    /// N4: the self-test's verdict and a graceful stop's give-back never
    /// both run, whichever order they come in.
    #[test]
    fn the_verdict_or_the_stop_has_the_last_word() {
        // The verdict first: the stop gives nothing back afterwards.
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        let v = Verdict::default();
        assert_eq!(v.decide(|| passed(&f, &f.data())), Some(()));
        assert!(!v.stop(|| stopped(&f, &f.data())));
        assert!(f.journal().is_none() && f.at(PREVIOUS).is_none());
        // The stop first: the verdict doesn't run, and the old program
        // stays behind the attempt it gave back.
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        let v = Verdict::default();
        assert!(v.stop(|| stopped(&f, &f.data())));
        assert_eq!(v.decide(|| passed(&f, &f.data())), None);
        let j = f.journal().unwrap();
        assert_eq!((j.state, j.attempts), (State::Swapped, 0));
        assert_eq!(f.at(PREVIOUS).as_deref(), Some(OLD));
    }

    /// N4: the lock is held for the whole verdict (a stop can't get in
    /// part way, pinned by `try_lock`), and a stop started meanwhile from
    /// another thread, whenever it runs, then does nothing: it never
    /// writes `Swapped` back over the journal `passed` just cleared.
    #[test]
    fn a_stop_during_the_verdict_waits_and_then_does_nothing() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        let v = Verdict::default();
        let gave_back = std::sync::Arc::new(AtomicBool::new(false));
        let mut stopper = None;
        v.decide(|| {
            // In the middle of the verdict: the lock is held, and a stop
            // started now can't get in.
            assert!(v.0.try_lock().is_err());
            let (v2, g) = (v.clone(), gave_back.clone());
            stopper = Some(std::thread::spawn(move || {
                v2.stop(|| g.store(true, Ordering::SeqCst))
            }));
            passed(&f, &f.data());
            assert!(!gave_back.load(Ordering::SeqCst));
        })
        .unwrap();
        assert!(
            !stopper.unwrap().join().unwrap(),
            "the stop ran after the verdict"
        );
        assert!(!gave_back.load(Ordering::SeqCst));
        assert!(f.journal().is_none());
    }

    #[test]
    fn a_second_copy_starting_during_probation_judges_nothing() {
        let f = Fake::new();
        f.install().unwrap();
        f.run_as("0.5.5");
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        // The first copy's notifier holds `.lock`.
        let held = f.bin().lock().unwrap().unwrap();
        for _ in 0..5 {
            assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Normal);
        }
        let j = f.journal().unwrap();
        assert_eq!((j.state, j.attempts), (State::Swapped, 1));
        assert_eq!(f.program_bytes().as_deref(), Some(NEW));
        drop(held);
        assert_eq!(at_start(&f, &f.data(), &f.db()), Start::Probation);
        assert_eq!(f.journal().unwrap().attempts, 2);
    }

    /// The launcher's way out of the one gap: with no `lodestar.exe` and a
    /// `previous.exe`, it moves that back before it starts anything, and
    /// never over an existing program. No labels: cmd can miss them in a
    /// file with Unix line endings (#651). (Line endings themselves aren't
    /// checked: a Windows checkout may convert them.)
    #[test]
    fn the_launcher_puts_the_previous_program_back_after_an_interrupted_update() {
        let cmd = include_str!("../../../../../packaging/windows/portable/Start Lodestar.cmd");
        let guard = r#"if not exist "%~dp0lodestar.exe" if exist "%~dp0data\bin\previous.exe" move "%~dp0data\bin\previous.exe" "%~dp0lodestar.exe""#;
        let at = cmd.find(guard).expect("the fallback's guarded move");
        let serve = cmd.find(r#""%~dp0lodestar.exe" serve"#).unwrap();
        assert!(at < serve, "the fallback runs before the program starts");
        assert!(
            !cmd.lines().any(|l| l.trim_start().starts_with(':')),
            "no labels"
        );
        for other in [NEXT, FAILED] {
            assert!(!cmd.contains(other), "the launcher never touches {other}");
        }
    }

    #[test]
    fn creates_a_missing_bin_and_leaves_an_existing_one() {
        let f = Fake::new();
        std::fs::remove_dir(f.data().join("bin")).unwrap();
        ensure_bin(&f, &f.data()).unwrap();
        assert!(f.data().join("bin").is_dir());
        std::fs::write(f.data().join("bin").join("keep"), b"").unwrap();
        ensure_bin(&f, &f.data()).unwrap();
        assert!(f.data().join("bin").join("keep").exists());
    }

    #[test]
    fn nothing_happens_outside_a_portable_folder() {
        let f = Fake::new();
        crashed(&f, State::Swapped, Some(NEW), Some(OLD));
        f.run_as("0.5.5");
        let other = f.app.join("other-data");
        std::fs::create_dir_all(other.join("bin")).unwrap();
        assert_eq!(at_start(&f, &other, &f.db()), Start::Normal);
        assert_eq!(f.journal().unwrap().attempts, 0);
    }

    /// A clock that moves only when the self-test pauses.
    struct Fake500(Cell<Duration>);

    impl Clock for Fake500 {
        fn elapsed(&self) -> Duration {
            self.0.get()
        }
        fn pause(&self) -> impl std::future::Future<Output = ()> + Send {
            self.0.set(self.0.get() + Duration::from_millis(500));
            std::future::ready(())
        }
    }

    #[tokio::test]
    async fn the_self_test_has_five_minutes() {
        // Passing on the last check inside the window counts.
        let clock = Fake500(Cell::new(Duration::ZERO));
        let tries = Cell::new(0u32);
        let check = || {
            tries.set(tries.get() + 1);
            let n = tries.get();
            async move { if n <= 600 { Err("/healthz") } else { Ok(()) } }
        };
        assert_eq!(self_test(check, PROBATION, &clock).await, Ok(()));
        assert_eq!(clock.elapsed(), PROBATION);
        // Never passing: the last check's name, once the window is over.
        let clock = Fake500(Cell::new(Duration::ZERO));
        let tries = Cell::new(0u32);
        let never = || {
            tries.set(tries.get() + 1);
            async { Err("the search index") }
        };
        assert_eq!(
            self_test(never, PROBATION, &clock).await,
            Err("the search index")
        );
        assert_eq!(clock.elapsed(), PROBATION);
        assert_eq!(tries.get(), 601);
        // The real clock's window is the same 5 minutes.
        assert_eq!(PROBATION, Duration::from_secs(300));
    }
}
