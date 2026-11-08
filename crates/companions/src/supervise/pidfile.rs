//! Records of running companions, so none outlives the Lodestar that
//! started it (plan §4 CMP-4a, the orphans design point).
//!
//! Each running companion has one record, `<data>/companions/run/<id>.json`:
//! `{"format":1,"pid":…,"start":…,"exe":"…"}`. `start` is the process's
//! start time as the OS reports it (Linux: clock ticks since boot from
//! `/proc/<pid>/stat`; Windows: the FILETIME of `Process.StartTime`), and
//! `exe` the executable's path. A record is created new, 0600 on Unix,
//! never through a link, in a 0700 folder that must be a real directory.
//!
//! - **At start-up** ([`kill_orphans`]), before any companion starts, a
//!   recorded process whose pid, start time **and** executable all still
//!   match is killed; every record is then removed. A partial match is
//!   never killed: the pid was reused.
//! - **When the work child dies** (Linux), the init calls [`kill_adopted`],
//!   which also requires the process to be the init's own child now. The
//!   init is single-threaded and reaps only between its own steps, so an
//!   adopted child can't be reaped, and its pid reused, between the check
//!   and the kill.
//! - **Race-free kill on Linux.** A pidfd is opened first, then the
//!   process is checked, then the signal goes through the pidfd: if the
//!   pid was reused after the open, the signal reaches nobody. Kernels
//!   without pidfds fall back to `kill(2)` after the same check.
//! - **Other platforms** have no identity here, so nothing is recorded and
//!   nothing is ever killed.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::inputs::CompanionId;

const FORMAT: u32 = 1;
const MAX_RECORD: u64 = 8 * 1024;
const MAX_RECORDS: usize = 64;

/// What identifies a running process beyond its pid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub format: u32,
    pub pid: u32,
    pub start: u64,
    pub exe: PathBuf,
}

/// Why a record couldn't be written or the folder read. No paths or
/// process details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PidfileError {
    /// `<data>/companions/run` isn't a real directory.
    NotADirectory,
    /// The process's identity can't be read on this platform or right now.
    NoIdentity,
    Io(io::ErrorKind),
}

impl PidfileError {
    pub fn code(&self) -> &'static str {
        match self {
            PidfileError::NotADirectory => "run_dir_not_a_directory",
            PidfileError::NoIdentity => "no_identity",
            PidfileError::Io(_) => "io",
        }
    }
}

impl fmt::Display for PidfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PidfileError::Io(k) => write!(f, "companion records: {k}"),
            other => write!(f, "companion records: {}", other.code()),
        }
    }
}

impl std::error::Error for PidfileError {}

impl From<io::Error> for PidfileError {
    fn from(e: io::Error) -> PidfileError {
        PidfileError::Io(e.kind())
    }
}

/// What a cleanup did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cleanup {
    /// Records whose process matched and was signalled.
    pub killed: usize,
    /// Records that didn't match (gone, or the pid reused) or were
    /// unreadable.
    pub stale: usize,
    /// Other adopted processes killed because they were in a recorded
    /// companion's process group ([`kill_adopted`] only).
    pub group: usize,
}

/// `<data>/companions/run`.
pub fn run_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("companions").join("run")
}

/// Creates `dir` 0700 if it's missing (never through a link) and checks
/// it's a real directory.
pub(crate) fn private_dir(dir: &Path) -> Result<(), PidfileError> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(PidfileError::NotADirectory);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Records a companion that just started. An old record for `id` is
/// replaced.
pub fn record(data_dir: &Path, id: &CompanionId, pid: u32) -> Result<Record, PidfileError> {
    let (start, exe) = identity(pid).ok_or(PidfileError::NoIdentity)?;
    let dir = run_dir(data_dir);
    private_dir(&dir)?;
    let rec = Record {
        format: FORMAT,
        pid,
        start,
        exe,
    };
    let path = dir.join(format!("{}.json", id.as_str()));
    match fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut file = opts.open(&path)?;
    let json =
        serde_json::to_vec(&rec).map_err(|_| PidfileError::Io(io::ErrorKind::InvalidData))?;
    file.write_all(&json)?;
    file.sync_all()?;
    Ok(rec)
}

/// Removes `id`'s record (it exited and was reaped).
pub fn remove(data_dir: &Path, id: &CompanionId) {
    let _ = fs::remove_file(run_dir(data_dir).join(format!("{}.json", id.as_str())));
}

/// The records in the folder, with their paths. A record that can't be
/// read is `None`.
fn records(dir: &Path) -> Result<Vec<(PathBuf, Option<Record>)>, PidfileError> {
    let meta = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other?,
    };
    if !meta.file_type().is_dir() {
        return Err(PidfileError::NotADirectory);
    }
    let mut out = Vec::new();
    let mut entries = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"));
    for path in entries.by_ref().take(MAX_RECORDS) {
        let rec = read_record(&path);
        out.push((path, rec));
    }
    Ok(out)
}

/// Opens a record without following a link, and without blocking on a
/// FIFO put in its place; checks the open handle is a small regular file.
fn read_record(path: &Path) -> Option<Record> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
        );
    }
    #[cfg(not(unix))]
    if !fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let file = opts.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_RECORD {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_RECORD).read_to_string(&mut text).ok()?;
    serde_json::from_str::<Record>(&text)
        .ok()
        .filter(|r| r.format == FORMAT)
}

/// At start-up, before any companion starts: kills each recorded process
/// whose pid, start time and executable still match, and removes every
/// record.
pub fn kill_orphans(data_dir: &Path) -> Result<Cleanup, PidfileError> {
    let mut done = Cleanup::default();
    for (path, rec) in records(&run_dir(data_dir))? {
        match rec {
            Some(r) if allowed(&r, data_dir) && kill_if_same(&r, None) => done.killed += 1,
            _ => done.stale += 1,
        }
        let _ = fs::remove_file(&path);
    }
    Ok(done)
}

/// For the init when its work child has died (Linux): kills each recorded
/// companion that matches **and** is now `parent`'s own child (adopted by
/// the subreaper), with its process group, and any other adopted child
/// still in a recorded companion's group. Records stay for the next start-up's
/// [`kill_orphans`]. `parent` must not reap concurrently with this call.
pub fn kill_adopted(data_dir: &Path, parent: u32) -> Result<Cleanup, PidfileError> {
    let mut done = Cleanup::default();
    for (_, rec) in records(&run_dir(data_dir))? {
        let rec = rec.filter(|r| allowed(r, data_dir));
        match &rec {
            Some(r) if kill_if_same(r, Some(parent)) => done.killed += 1,
            _ => done.stale += 1,
        }
        #[cfg(target_os = "linux")]
        if let Some(r) = &rec {
            done.group += kill_adopted_group(parent, r.pid);
        }
    }
    Ok(done)
}

/// Whether a record may lead to a kill at all, before its identity is
/// checked: never Lodestar itself, its parent, its group, pid 1 or below
/// (a group kill of 1 is `kill(-1)`: everything), and only a program
/// installed under `<data>/companions/`. So the identity check is also an
/// authorisation check: a record can't name an arbitrary program.
fn allowed(rec: &Record, data_dir: &Path) -> bool {
    !forbidden_pid(rec.pid) && installed(&rec.exe, data_dir)
}

/// Pids no record may name.
pub(crate) fn forbidden_pid(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        let raw = |p: Option<rustix::process::Pid>| p.map(|p| p.as_raw_nonzero().get() as u32);
        pid <= 1
            || Some(pid) == raw(rustix::process::getppid())
            || Some(pid) == raw(Some(rustix::process::getpgrp()))
    }
    #[cfg(not(unix))]
    {
        // Windows: 0 is the idle process and 4 the System process.
        pid <= 4
    }
}

/// Whether `exe` is under `<data>/companions/`, compared canonically. A
/// program deleted since it started can't be resolved; its path as the OS
/// reported it is used then, and must have no `..`.
fn installed(exe: &Path, data_dir: &Path) -> bool {
    let Ok(root) = fs::canonicalize(data_dir.join("companions")) else {
        return false;
    };
    let exe = match fs::canonicalize(exe) {
        Ok(e) => e,
        Err(_)
            if exe.is_absolute()
                && !exe
                    .components()
                    .any(|c| c == std::path::Component::ParentDir) =>
        {
            exe.to_owned()
        }
        Err(_) => return false,
    };
    exe.starts_with(&root) && exe != root
}

/// Whether `pid` still is the process `rec` names (and `parent`'s child,
/// when given).
#[cfg(any(target_os = "linux", windows))]
fn matches(rec: &Record, parent: Option<u32>) -> bool {
    if parent.is_some() && parent != parent_of(rec.pid) {
        return false;
    }
    identity(rec.pid).is_some_and(|(start, exe)| start == rec.start && exe == rec.exe)
}

#[cfg(target_os = "linux")]
fn kill_if_same(rec: &Record, parent: Option<u32>) -> bool {
    use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
    let Some(pid) = i32::try_from(rec.pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    match pidfd_open(pid, PidfdFlags::empty()) {
        // The pidfd pins this process: a reuse after this point makes the
        // signal fail rather than reach the new one.
        Ok(fd) => {
            if !matches(rec, parent) {
                return false;
            }
            // Its process group (whatever it started) first, while the
            // leader is alive and holds the group's id; then the leader
            // itself through the pidfd.
            kill_own_group(rec.pid);
            pidfd_send_signal(&fd, Signal::KILL).is_ok()
        }
        Err(rustix::io::Errno::NOSYS) => {
            if !matches(rec, parent) {
                return false;
            }
            kill_own_group(rec.pid);
            rustix::process::kill_process(pid, Signal::KILL).is_ok()
        }
        Err(_) => false,
    }
}

/// Kills the process group `pid` leads, only if it does lead one: its
/// group id (`/proc/<pid>/stat` field 5) is its own pid.
#[cfg(target_os = "linux")]
fn kill_own_group(pid: u32) {
    let leads = stat_field(pid, 5).and_then(|g| g.parse::<u32>().ok()) == Some(pid);
    if leads
        && !forbidden_pid(pid)
        && let Some(p) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(p, rustix::process::Signal::KILL);
    }
}

/// Kills `parent`'s own children in the process group `pgid`: what a
/// companion started, adopted by the init after both the work child and
/// the companion died. Being `parent`'s children, none can be reaped (and
/// its pid reused) before `parent` reaps again.
#[cfg(target_os = "linux")]
fn kill_adopted_group(parent: u32, pgid: u32) -> usize {
    use rustix::process::{Pid, Signal};
    if forbidden_pid(pgid) {
        return 0;
    }
    let Ok(dir) = fs::read_dir("/proc") else {
        return 0;
    };
    let mut killed = 0;
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let ours = parent_of(pid) == Some(parent)
            && stat_field(pid, 5).and_then(|g| g.parse::<u32>().ok()) == Some(pgid);
        if ours
            && let Some(p) = i32::try_from(pid).ok().and_then(Pid::from_raw)
            && rustix::process::kill_process(p, Signal::KILL).is_ok()
        {
            killed += 1;
        }
    }
    killed
}

#[cfg(windows)]
fn kill_if_same(rec: &Record, parent: Option<u32>) -> bool {
    if parent.is_some() || !matches(rec, None) {
        return false;
    }
    // Checked again inside the one PowerShell call that kills, against the
    // start time, so a pid reused since is left alone.
    let script = format!(
        "$p = Get-Process -Id {} -ErrorAction Stop; $null = $p.Handle; \
         if ($p.StartTime.ToFileTimeUtc() -eq {}) {{ $p.Kill(); 'killed' }}",
        rec.pid, rec.start
    );
    windows::powershell(&script).is_some_and(|out| out.trim() == "killed")
}

#[cfg(not(any(target_os = "linux", windows)))]
fn kill_if_same(_: &Record, _: Option<u32>) -> bool {
    false
}

/// A process's start time and executable path.
#[cfg(target_os = "linux")]
pub fn identity(pid: u32) -> Option<(u64, PathBuf)> {
    let start = stat_field(pid, 22)?.parse().ok()?;
    let exe = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    // A running program whose file was since deleted (an old version
    // swept) reads "<path> (deleted)"; the start time still pins it.
    let exe = match exe.to_str().and_then(|e| e.strip_suffix(" (deleted)")) {
        Some(e) => PathBuf::from(e),
        None => exe,
    };
    Some((start, exe))
}

#[cfg(windows)]
pub fn identity(pid: u32) -> Option<(u64, PathBuf)> {
    let script = format!(
        "$p = Get-Process -Id {pid} -ErrorAction Stop; $p.StartTime.ToFileTimeUtc(); $p.Path"
    );
    let out = windows::powershell(&script)?;
    let mut lines = out.lines().map(str::trim);
    let start = lines.next()?.parse().ok()?;
    let exe = lines.next().filter(|p| !p.is_empty())?;
    Some((start, PathBuf::from(exe)))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn identity(_: u32) -> Option<(u64, PathBuf)> {
    None
}

/// Field `n` (1-based, as `proc(5)` numbers them) of `/proc/<pid>/stat`.
/// The command name (field 2) can hold spaces and `)`, so fields are
/// counted from after its last `)`.
#[cfg(target_os = "linux")]
fn stat_field(pid: u32, n: usize) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace()
        .nth(n.checked_sub(3)?)
        .map(str::to_owned)
}

#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    stat_field(pid, 4)?.parse().ok()
}

#[cfg(windows)]
fn parent_of(_: u32) -> Option<u32> {
    None
}

#[cfg(windows)]
pub(crate) mod windows {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// `CREATE_NO_WINDOW`.
    const NO_WINDOW: u32 = 0x0800_0000;

    /// How long one PowerShell call may take before it's killed.
    const LIMIT: Duration = Duration::from_secs(30);

    /// Runs Windows PowerShell 5.1 from System32 by absolute path; its
    /// standard output, or `None` if it failed or overran [`LIMIT`]. It
    /// blocks: call it from a blocking thread.
    pub(crate) fn powershell(script: &str) -> Option<String> {
        let root = std::env::var_os("SystemRoot")?;
        let exe = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let mut child = Command::new(exe)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env_remove("PSModulePath")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(NO_WINDOW)
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        // Read on its own thread so a chatty script can't fill the pipe
        // while this one waits.
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.take(64 * 1024).read_to_end(&mut out);
            out
        });
        let until = Instant::now() + LIMIT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        let out = reader.join().ok()?;
        if !status.success() {
            return None;
        }
        String::from_utf8(out).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_and_refuses_unknown_fields() {
        let r = Record {
            format: 1,
            pid: 7,
            start: 99,
            exe: PathBuf::from("/x/y"),
        };
        let json = serde_json::to_string(&r).unwrap_or_default();
        assert_eq!(serde_json::from_str::<Record>(&json).ok(), Some(r));
        assert!(
            serde_json::from_str::<Record>(r#"{"format":1,"pid":7,"start":9,"exe":"/x","x":1}"#)
                .is_err()
        );
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn this_process_has_an_identity() {
        let (start, exe) = identity(std::process::id()).unwrap_or_default();
        assert!(start > 0);
        let me = std::env::current_exe().unwrap_or_default();
        assert_eq!(
            fs::canonicalize(&exe).unwrap_or_default(),
            fs::canonicalize(me).unwrap_or_default()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_stat_fields_are_counted_after_the_name() {
        assert_eq!(
            stat_field(std::process::id(), 3).as_deref().map(str::len),
            Some(1)
        );
        assert_eq!(
            parent_of(std::process::id()),
            rustix::process::getppid().map(|p| p.as_raw_nonzero().get() as u32)
        );
    }

    #[test]
    fn lodestar_its_parent_group_and_low_pids_are_never_named() {
        assert!(forbidden_pid(0));
        assert!(forbidden_pid(1));
        assert!(forbidden_pid(std::process::id()));
        #[cfg(unix)]
        {
            let raw = |p: rustix::process::Pid| p.as_raw_nonzero().get() as u32;
            if let Some(pp) = rustix::process::getppid() {
                assert!(forbidden_pid(raw(pp)));
            }
            assert!(forbidden_pid(raw(rustix::process::getpgrp())));
        }
    }

    #[test]
    fn only_a_program_under_companions_may_be_killed() {
        let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let bin = data.path().join("companions").join("x");
        fs::create_dir_all(&bin).unwrap_or_else(|e| panic!("{e}"));
        fs::write(bin.join("prog"), b"").unwrap_or_else(|e| panic!("{e}"));
        let root = fs::canonicalize(data.path()).unwrap_or_else(|e| panic!("{e}"));
        let rec = |exe: PathBuf, pid: u32| Record {
            format: FORMAT,
            pid,
            start: 1,
            exe,
        };
        let good = root.join("companions").join("x").join("prog");
        assert!(installed(&good, data.path()));
        assert!(allowed(&rec(good.clone(), 999_999), data.path()));
        // Gone, but reported plainly under the folder.
        assert!(installed(
            &root.join("companions").join("x").join("gone"),
            data.path()
        ));
        // Outside it, or climbing out, or the folder itself.
        assert!(!installed(&root.join("elsewhere"), data.path()));
        assert!(!installed(
            &root.join("companions").join("..").join("gone"),
            data.path()
        ));
        assert!(!installed(&root.join("companions"), data.path()));
        assert!(!installed(
            &std::env::current_exe().unwrap_or_default(),
            data.path()
        ));
        // A good program under a forbidden pid.
        assert!(!allowed(&rec(good.clone(), 1), data.path()));
        assert!(!allowed(&rec(good, std::process::id()), data.path()));
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_a_record_does_not_block() {
        let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let fifo = data.path().join("a.json");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_record(&fifo), None);
    }

    #[test]
    fn unreadable_and_foreign_records_are_stale_and_removed() {
        let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let dir = run_dir(data.path());
        assert_eq!(
            private_dir(&dir),
            Err(PidfileError::Io(io::ErrorKind::NotFound))
        );
        fs::create_dir_all(dir.parent().unwrap_or(&dir)).unwrap_or_else(|e| panic!("{e}"));
        private_dir(&dir).unwrap_or_else(|e| panic!("{e}"));
        fs::write(dir.join("a.json"), "not json").unwrap_or_else(|e| panic!("{e}"));
        fs::write(dir.join("notes.txt"), "kept").unwrap_or_else(|e| panic!("{e}"));
        let done = kill_orphans(data.path()).unwrap_or_default();
        assert_eq!(
            done,
            Cleanup {
                killed: 0,
                stale: 1,
                group: 0
            }
        );
        assert!(!dir.join("a.json").exists());
        assert!(dir.join("notes.txt").exists());
    }
}
