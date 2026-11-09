//! The MSI's probation and rollback (PK-win-upd-3; architecture §12.2,
//! plan Q13 rule 2, B-update-r2's store-before-MSI order), run by the
//! update helper as SYSTEM.
//!
//! **The journal** is `journal.json` in the helper's staging folder, which
//! only SYSTEM and Administrators can write: nothing in the data folder
//! decides a rollback (`result.json` there is display only). It's written
//! by temporary file and rename before each step, so a crash or a restart
//! at any point is finished by the next run of the helper, at boot or on
//! demand ([`resume`]), before it looks at any new request.
//!
//! 1. **Installing:** written before `msiexec` runs, with the version
//!    installed from, the one installed and the store's snapshots then.
//!    Probation follows: the new service must reach Running and stay
//!    there ([`super::helper::System::healthy`]). Passed, the journal goes.
//!    After an install that needs Windows to restart (exit 3010) the
//!    journal stays, and probation is judged after the restart.
//! 2. **Rolling back** (an unhealthy probation), each step idempotent:
//!    1. choose the snapshot to restore: the earliest pre-migration
//!       snapshot the failed version took (one not there before the
//!       install), by file name; its SHA-256 is recorded;
//!    2. set the service to manual start, so the SCM can't restart a half
//!       rolled-back install, stop it, refuse while anything still runs as
//!       its account, and mark the failed version `failed` in `skipped`
//!       under `.lock`;
//!    3. **the store first:** the snapshot renamed over `lodestar.db`
//!       (its `-wal`, `-shm` and `-journal` removed), beneath the pinned
//!       data folder. A repeat that finds the snapshot gone and the store
//!       at its SHA-256 counts it done;
//!    4. **then the previous MSI,** from staging's `previous\`, checked
//!       against the pins kept with it (the journal's rollback target is
//!       the one older version the helper installs, Q13 rule 4);
//!    5. the service back to delayed automatic start, and started.
//!
//!    A rollback that can't go on (no previous MSI, a tampered one, a
//!    store that doesn't match) halts: the journal records why, nothing
//!    more is tried, and the service is set back to automatic start.

use lodestar_store::snapshots::{self, Kind as SnapKind};
use lodestar_update::semver::Version;
use serde::{Deserialize, Serialize};

use super::helper::{self, Context, Outcome, System};
use super::msi::{self, PINS, Pins, Refusal};
use crate::update::layout::{self, Bin};

/// In staging.
pub const JOURNAL: &str = "journal.json";
const FORMAT: u32 = 1;
/// The store, in the data folder (the default `Config::db_path`).
const STORE: &str = "lodestar.db";

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
    pub state: State,
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
    /// It couldn't go on; nothing more is tried.
    Halted { why: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRef {
    /// A file name beside the store, never a path.
    pub name: String,
    pub sha256: String,
}

/// The journal in staging, if any. One that can't be read is a halt:
/// the run refuses rather than guess.
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

/// The names of the store's snapshots now.
pub fn snapshot_names(ctx: &Context) -> Vec<String> {
    snapshots::list(&ctx.data_dir.join(STORE))
        .map(|all| all.into_iter().map(|s| s.name).collect())
        .unwrap_or_default()
}

/// The journal written before `msiexec` runs.
pub fn installing(ctx: &Context, id: &str, from: &Version, to: &Version) -> Journal {
    Journal {
        format: FORMAT,
        id: id.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        snapshots_before: snapshot_names(ctx),
        state: State::Installing,
    }
}

/// Probation for an installed version, then a rollback if it fails.
/// The journal is in place (`Installing`).
pub fn probation(ctx: &Context, sys: &dyn System, journal: Journal, restart: bool) -> Outcome {
    let to = Version::parse(&journal.to).unwrap_or_else(|_| Version::new(0, 0, 0));
    if restart {
        // The old file may run until Windows restarts: judged at boot.
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

/// Finishes what a journal left: probation after a restart or a crash,
/// or a rollback. `None` when there's no journal. Run first, before any
/// request.
pub fn resume(ctx: &Context, sys: &dyn System) -> Result<Option<Outcome>, Refusal> {
    let Some(journal) = read(ctx)? else {
        return Ok(None);
    };
    Ok(Some(match journal.state {
        State::Installing => probation(ctx, sys, journal, false),
        State::RollingBack { .. } => roll_back(ctx, sys, journal),
        State::Halted { ref why } => Outcome::RollbackHalted { why: why.clone() },
    }))
}

/// The rollback, from wherever the journal stands.
fn roll_back(ctx: &Context, sys: &dyn System, mut j: Journal) -> Outcome {
    match steps(ctx, sys, &mut j) {
        Ok(()) => {
            clear(ctx);
            Outcome::RolledBack {
                failed: j.to.clone(),
                to: j.from.clone(),
            }
        }
        Err(why) => {
            j.state = State::Halted { why: why.clone() };
            let _ = write(ctx, &j);
            // Never left on manual: the SCM keeps it running.
            let _ = sys.set_manual(false);
            sys.start_service();
            Outcome::RollbackHalted { why }
        }
    }
}

fn steps(ctx: &Context, sys: &dyn System, j: &mut Journal) -> Result<(), String> {
    let (mut snapshot, mut done) = match &j.state {
        State::RollingBack { snapshot, done } => (snapshot.clone(), *done),
        _ => (None, 0),
    };
    let save = |j: &mut Journal, snapshot: &Option<SnapshotRef>, done: u8| {
        j.state = State::RollingBack {
            snapshot: snapshot.clone(),
            done,
        };
        write(ctx, j).map_err(|e| e.to_string())
    };
    // The target first: without it nothing is touched.
    previous_msi(ctx, &j.from)?;
    if done < 1 {
        snapshot = choose(ctx, &j.snapshots_before)?;
        done = 1;
        save(j, &snapshot, done)?;
    }
    if done < 2 {
        sys.set_manual(true)
            .map_err(|e| format!("setting the service to manual start: {e}"))?;
        sys.stop_service()
            .map_err(|e| format!("the service didn't stop: {e}"))?;
        no_service_processes(sys)?;
        let bin = Bin::existing(&ctx.data_dir)
            .map_err(|e| format!("bin: {e}"))?
            .ok_or("bin is missing")?;
        let lock = helper::take_lock(&bin, ctx.lock_wait).map_err(|e| e.to_string())?;
        super::check::add_skip(&bin, &j.to, super::check::SKIP_FAILED)
            .map_err(|e| format!("skipped: {e}"))?;
        drop(lock);
        done = 2;
        save(j, &snapshot, done)?;
    }
    if done < 3 {
        if let Some(s) = &snapshot {
            restore(ctx, s)?;
        }
        done = 3;
        save(j, &snapshot, done)?;
    }
    if done < 4 {
        // Checked again right before it runs.
        let staged = previous_msi(ctx, &j.from)?;
        let args = msi::msiexec_args(&staged, &ctx.staging.log());
        let code = sys.msiexec(&args).map_err(|e| format!("msiexec: {e}"))?;
        if !matches!(
            msi::installed(code),
            msi::Installed::Done | msi::Installed::RestartNeeded
        ) {
            return Err(format!("reinstalling {} failed ({code})", j.from));
        }
        done = 4;
        save(j, &snapshot, done)?;
    }
    if done < 5 {
        sys.set_manual(false)
            .map_err(|e| format!("setting the service back to automatic start: {e}"))?;
        sys.start_service();
        rotate_back(ctx);
        done = 5;
        save(j, &snapshot, done)?;
    }
    Ok(())
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

/// The MSI for `from` in staging's `previous\`, checked against its pins
/// (version, size, SHA-256). Anything else halts.
fn previous_msi(ctx: &Context, from: &str) -> Result<std::path::PathBuf, String> {
    let dir = ctx.staging.previous();
    let pins: Pins = layout::read_json(&dir.join(PINS))
        .map_err(|e| format!("the previous MSI's pins can't be read: {e}"))?
        .ok_or_else(|| format!("no MSI for {from} is kept, so it can't be reinstalled"))?;
    if pins.version != from {
        return Err(format!("the kept MSI is {}, not {from}", pins.version));
    }
    let v = layout::parse_version(&pins.version).ok_or("the kept pins' version is invalid")?;
    let path = dir.join(msi::msi_name(&v));
    let (sha256, size) = helper::hash(&path).map_err(|e| format!("the kept MSI: {e}"))?;
    if size != pins.size || sha256.to_hex() != pins.sha256 {
        return Err("the kept MSI doesn't match its pins".into());
    }
    Ok(path)
}

/// The snapshot to restore: the earliest pre-migration snapshot that
/// wasn't there before the install (the failed version's first
/// migration), or none when it took none (the store is kept). Chosen by
/// name alone: SYSTEM never opens the store. The old version checks it
/// when it opens it.
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
    let (sha, _) = helper::hash(&s.path).map_err(|e| format!("{}: {e}", s.name))?;
    Ok(Some(SnapshotRef {
        name: s.name,
        sha256: sha.to_hex(),
    }))
}

/// Step 3: the snapshot over the store, beneath the pinned data folder.
fn restore(ctx: &Context, s: &SnapshotRef) -> Result<(), String> {
    let _pin = layout::pin_dir(&ctx.data_dir).map_err(|e| format!("the data folder: {e}"))?;
    let dir = &ctx.data_dir;
    if s.name.contains(['/', '\\']) || snapshots::parse_name("lodestar", &s.name).is_none() {
        return Err("the snapshot's name isn't one beside the store".into());
    }
    let snap = dir.join(&s.name);
    let store = dir.join(STORE);
    if std::fs::symlink_metadata(&snap).is_err() {
        // Gone: done only if the store is that snapshot.
        let (sha, _) = helper::hash(&store).map_err(|e| format!("the store: {e}"))?;
        return if sha.to_hex() == s.sha256 {
            Ok(())
        } else {
            Err(format!(
                "the snapshot {} is gone and the store isn't it",
                s.name
            ))
        };
    }
    let (sha, _) = helper::hash(&snap).map_err(|e| format!("{}: {e}", s.name))?;
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
    layout::rename_retrying(&snap, &store).map_err(|e| format!("restoring the store: {e}"))
}

/// After a rollback, the reinstalled MSI is the current one again.
fn rotate_back(ctx: &Context) {
    let (current, previous) = (ctx.staging.current(), ctx.staging.previous());
    let _ = helper::remove_dir(&current);
    let _ = layout::rename_retrying(&previous, &current);
}
