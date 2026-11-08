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
}

/// `<data>/companions/run`.
pub fn run_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("companions").join("run")
}

/// Creates `dir` 0700 if it's missing (never through a link) and checks
/// it's a real directory.
pub(crate) fn private_dir(dir: &Path) -> Result<(), PidfileError> {
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
    for entry in fs::read_dir(dir)?.take(MAX_RECORDS) {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        out.push((path.clone(), read_record(&path)));
    }
    Ok(out)
}

fn read_record(path: &Path) -> Option<Record> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_RECORD {
        return None;
    }
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_RECORD)
        .read_to_string(&mut text)
        .ok()?;
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
            Some(r) if kill_if_same(&r, None) => done.killed += 1,
            _ => done.stale += 1,
        }
        let _ = fs::remove_file(&path);
    }
    Ok(done)
}

/// For the init when its work child has died (Linux): kills each recorded
/// companion that matches **and** is now `parent`'s own child (adopted by
/// the subreaper). Records stay for the next start-up's
/// [`kill_orphans`]. `parent` must not reap concurrently with this call.
pub fn kill_adopted(data_dir: &Path, parent: u32) -> Result<Cleanup, PidfileError> {
    let mut done = Cleanup::default();
    for (_, rec) in records(&run_dir(data_dir))? {
        match rec {
            Some(r) if kill_if_same(&r, Some(parent)) => done.killed += 1,
            _ => done.stale += 1,
        }
    }
    Ok(done)
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
        Ok(fd) => matches(rec, parent) && pidfd_send_signal(&fd, Signal::KILL).is_ok(),
        Err(rustix::io::Errno::NOSYS) => {
            matches(rec, parent) && rustix::process::kill_process(pid, Signal::KILL).is_ok()
        }
        Err(_) => false,
    }
}

#[cfg(windows)]
fn kill_if_same(rec: &Record, parent: Option<u32>) -> bool {
    if parent.is_some() || !matches(rec, None) {
        return false;
    }
    // Checked again inside the one PowerShell call that kills, against the
    // start time, so a pid reused since is left alone.
    let script = format!(
        "$p = Get-Process -Id {} -ErrorAction Stop; \
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
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// `CREATE_NO_WINDOW`.
    const NO_WINDOW: u32 = 0x0800_0000;

    /// Runs Windows PowerShell 5.1 from System32 by absolute path; its
    /// standard output, or `None` if it failed.
    pub(crate) fn powershell(script: &str) -> Option<String> {
        let root = std::env::var_os("SystemRoot")?;
        let exe = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let out = Command::new(exe)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env_remove("PSModulePath")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(NO_WINDOW)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
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
                stale: 1
            }
        );
        assert!(!dir.join("a.json").exists());
        assert!(dir.join("notes.txt").exists());
    }
}
