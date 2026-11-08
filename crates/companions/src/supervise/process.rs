//! Starting one companion process (plan §4 CMP-4a, Q3, Q7).
//!
//! - **Environment.** Linux and other Unixes: cleared, then only the
//!   template's variables. Windows: the base a process needs
//!   (`SystemRoot`, `TEMP`/`TMP` and `USERPROFILE`/`APPDATA` in the
//!   companion's folder, `PATH` limited to the system directories), then
//!   the template's variables, which may not replace that base.
//! - **Working directory:** `<data>/companions/<id>/home/`, 0700 on Unix,
//!   created without following links.
//! - **Streams:** stdin is null; stdout and stderr are piped to the log
//!   ring ([`super::logs`]).
//! - **Executable:** an absolute path to a regular file (not a link), from
//!   CMP-2's layout (`Install::executable`). `PATH` is never searched.
//! - **Privilege:** never elevated. Lodestar refuses to start a companion
//!   while it runs as root or an administrator.
//! - **After spawn** an [`AfterSpawn`] hook gets the child (CMP-4b applies
//!   its limits there); a refusal kills the child.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::inputs::CompanionId;

use super::pidfile::{self, PidfileError};

/// A template's environment variable name: `[A-Za-z_][A-Za-z0-9_]{0,63}`,
/// and none that changes how programs load (`LD_*`, `DYLD_*`) or, on
/// Windows, the base Lodestar sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvName(String);

/// Names a template may not set.
const RESERVED: &[&str] = &[
    "PATH",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
];

impl EnvName {
    pub fn parse(name: &str) -> Result<EnvName, SpawnError> {
        let b = name.as_bytes();
        let ok = !b.is_empty()
            && b.len() <= 64
            && (b[0].is_ascii_alphabetic() || b[0] == b'_')
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_');
        let upper = name.to_ascii_uppercase();
        if !ok
            || upper.starts_with("LD_")
            || upper.starts_with("DYLD_")
            || RESERVED.contains(&upper.as_str())
        {
            return Err(SpawnError::BadEnvName);
        }
        Ok(EnvName(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What to start. Its `Debug` shows env names, never values.
#[derive(Clone)]
pub struct SpawnSpec {
    pub id: CompanionId,
    /// `Install::executable(version, entry's executable)`.
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    /// The template's variables. Values may hold credentials: they're
    /// never logged or put in an error.
    pub env: Vec<(EnvName, OsString)>,
    /// Lodestar's data folder.
    pub data_dir: PathBuf,
    /// Honoured only in test builds (`cfg(test)` or the `testing`
    /// feature), where CI runners are root or elevated: start anyway.
    /// Ignored in every other build, so shipped callers set `false`.
    pub allow_privileged: bool,
}

impl fmt::Debug for SpawnSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("SpawnSpec")
            .field("id", &self.id)
            .field("executable", &self.executable)
            .field("args", &self.args.len())
            .field("env", &names)
            .field("data_dir", &self.data_dir)
            .finish()
    }
}

/// Why a companion wasn't started. Codes only: no paths, env values or
/// output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnError {
    /// Lodestar runs as root or an administrator.
    Privileged,
    /// Whether Lodestar is an administrator couldn't be determined.
    PrivilegeUnknown,
    /// The executable isn't an absolute path to a regular file.
    BadExecutable,
    BadEnvName,
    /// The id is `run`, the records folder's name.
    ReservedId,
    /// The working directory couldn't be made private.
    WorkDir,
    /// The OS refused to start it.
    Start(io::ErrorKind),
    /// The [`AfterSpawn`] hook refused it; it was killed.
    Hook(&'static str),
    /// It couldn't be recorded for orphan cleanup; it was killed.
    Record(PidfileError),
}

impl SpawnError {
    pub fn code(&self) -> &'static str {
        match self {
            SpawnError::Privileged => "privileged",
            SpawnError::PrivilegeUnknown => "privilege_unknown",
            SpawnError::BadExecutable => "bad_executable",
            SpawnError::BadEnvName => "bad_env_name",
            SpawnError::ReservedId => "reserved_id",
            SpawnError::WorkDir => "work_dir",
            SpawnError::Start(_) => "start_failed",
            SpawnError::Hook(code) => code,
            SpawnError::Record(e) => e.code(),
        }
    }
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpawnError::Start(k) => write!(f, "the companion couldn't start: {k}"),
            other => write!(f, "the companion couldn't start: {}", other.code()),
        }
    }
}

impl std::error::Error for SpawnError {}

/// Runs right after a companion is spawned, before it's recorded or
/// supervised: CMP-4b's resource limits and sandbox. An error kills it.
pub trait AfterSpawn: Send + Sync {
    fn applied(&self, child: &tokio::process::Child) -> Result<(), &'static str>;
}

/// No hook.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHook;

impl AfterSpawn for NoHook {
    fn applied(&self, _: &tokio::process::Child) -> Result<(), &'static str> {
        Ok(())
    }
}

/// `<data>/companions/run` holds the records, so no companion may be
/// called `run`.
pub fn is_reserved(id: &CompanionId) -> bool {
    id.as_str() == "run"
}

/// `<data>/companions/<id>/home`: the companion's working directory and,
/// on Windows, its profile.
pub fn home_dir(data_dir: &Path, id: &CompanionId) -> PathBuf {
    data_dir.join("companions").join(id.as_str()).join("home")
}

/// Creates `dir` if missing (never through a link) and checks it's a real
/// directory.
pub(crate) fn real_dir(dir: &Path) -> io::Result<()> {
    match fs::create_dir(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    if fs::symlink_metadata(dir)?.file_type().is_dir() {
        Ok(())
    } else {
        Err(io::ErrorKind::NotADirectory.into())
    }
}

/// Creates the companion's folders down to `home` (0700) and returns it.
fn work_dir(data_dir: &Path, id: &CompanionId) -> Result<PathBuf, SpawnError> {
    let home = home_dir(data_dir, id);
    let made = || -> Result<(), PidfileError> {
        for dir in [
            data_dir.join("companions"),
            data_dir.join("companions").join(id.as_str()),
        ] {
            real_dir(&dir)?;
        }
        pidfile::private_dir(&home)?;
        #[cfg(windows)]
        pidfile::private_dir(&home.join("temp"))?;
        Ok(())
    };
    made().map_err(|_| SpawnError::WorkDir)?;
    Ok(home)
}

/// Whether this process runs as root or an administrator.
pub fn privileged() -> Result<bool, SpawnError> {
    #[cfg(unix)]
    {
        Ok(rustix::process::geteuid().is_root())
    }
    #[cfg(windows)]
    {
        // Only a definite answer is kept: a failed probe is asked again.
        static ADMIN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if let Some(admin) = ADMIN.get() {
            return Ok(*admin);
        }
        let script = "$i = [Security.Principal.WindowsIdentity]::GetCurrent(); \
             $p = [Security.Principal.WindowsPrincipal]::new($i); \
             $i.IsSystem -or $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)";
        let admin = match pidfile::windows::powershell(script)
            .as_deref()
            .map(str::trim)
        {
            Some("True") => true,
            Some("False") => false,
            _ => return Err(SpawnError::PrivilegeUnknown),
        };
        Ok(*ADMIN.get_or_init(|| admin))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(SpawnError::PrivilegeUnknown)
    }
}

/// The environment the companion gets.
fn environment(spec: &SpawnSpec, home: &Path) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        let sys = |sub: &str| {
            let mut p = PathBuf::from(&root);
            if !sub.is_empty() {
                p.push(sub);
            }
            p.into_os_string()
        };
        let path = std::env::join_paths([sys("System32"), sys(""), sys(r"System32\Wbem")])
            .unwrap_or_default();
        let temp = home.join("temp").into_os_string();
        env.extend([
            ("SystemRoot".into(), root.clone()),
            ("PATH".into(), path),
            ("TEMP".into(), temp.clone()),
            ("TMP".into(), temp),
            ("USERPROFILE".into(), home.as_os_str().to_owned()),
            ("APPDATA".into(), home.as_os_str().to_owned()),
        ]);
    }
    env.extend(
        spec.env
            .iter()
            .map(|(k, v)| (OsString::from(k.as_str()), v.clone())),
    );
    env
}

/// Kills the group a just-spawned, unreaped companion leads (Unix).
fn kill_group(pid: u32) {
    #[cfg(unix)]
    if let Some(p) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(p, rustix::process::Signal::KILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// A started companion: the child (stdout and stderr still piped) and
/// its pid.
#[derive(Debug)]
pub struct Started {
    pub child: tokio::process::Child,
    pub pid: u32,
}

/// Starts the companion: checks, folders, spawn, the hook, the record.
/// Must run inside a Tokio runtime, on a thread that may block (on
/// Windows the identity checks run PowerShell): the supervisor calls it
/// through `spawn_blocking`.
pub fn spawn(spec: &SpawnSpec, hook: &dyn AfterSpawn) -> Result<Started, SpawnError> {
    #[cfg(any(test, feature = "testing"))]
    let allow = spec.allow_privileged;
    #[cfg(not(any(test, feature = "testing")))]
    let allow = {
        let _ = spec.allow_privileged;
        false
    };
    if !allow && privileged()? {
        return Err(SpawnError::Privileged);
    }
    if is_reserved(&spec.id) {
        return Err(SpawnError::ReservedId);
    }
    let exe = &spec.executable;
    let regular = fs::symlink_metadata(exe).is_ok_and(|m| m.file_type().is_file());
    if !exe.is_absolute() || !regular {
        return Err(SpawnError::BadExecutable);
    }
    let home = work_dir(&spec.data_dir, &spec.id)?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(&spec.args)
        .env_clear()
        .envs(environment(spec, &home))
        .current_dir(&home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // If the supervising task is dropped, the companion dies with it.
        .kill_on_drop(true);
    // Its own process group, so a stop or kill reaches whatever it starts
    // (FlareSolverr's browser). Windows uses CMP-4b's Job Object instead.
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().map_err(|e| SpawnError::Start(e.kind()))?;
    let Some(pid) = child.id() else {
        return Err(SpawnError::Start(io::ErrorKind::Other));
    };
    if let Err(code) = hook.applied(&child) {
        kill_group(pid);
        let _ = child.start_kill();
        return Err(SpawnError::Hook(code));
    }
    if let Err(e) = pidfile::record(&spec.data_dir, &spec.id, pid) {
        // Unsupported platforms have no identity: nothing to record, and
        // nothing would ever be killed from a record. One that already
        // exited has none either, and can't be orphaned: its status is
        // kept for the caller's `wait`.
        let exited = matches!(child.try_wait(), Ok(Some(_)));
        let unsupported = !cfg!(any(target_os = "linux", windows));
        if !(e == PidfileError::NoIdentity && (exited || unsupported)) {
            kill_group(pid);
            let _ = child.start_kill();
            return Err(SpawnError::Record(e));
        }
    }
    Ok(Started { child, pid })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names_are_checked() {
        for ok in ["QBT_PROFILE", "_X", "a1"] {
            assert!(EnvName::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "1A",
            "A-B",
            "A B",
            "LD_PRELOAD",
            "ld_library_path",
            "DYLD_X",
            "Path",
            "TEMP",
            "SystemRoot",
        ] {
            assert_eq!(EnvName::parse(bad), Err(SpawnError::BadEnvName), "{bad}");
        }
        assert!(EnvName::parse(&"A".repeat(65)).is_err());
    }

    #[test]
    fn errors_are_codes() {
        assert_eq!(
            SpawnError::Start(io::ErrorKind::NotFound).code(),
            "start_failed"
        );
        assert_eq!(SpawnError::Hook("no_job").code(), "no_job");
        assert_eq!(
            SpawnError::Privileged.to_string(),
            "the companion couldn't start: privileged"
        );
    }

    #[cfg(unix)]
    #[test]
    fn privileged_means_root() {
        assert_eq!(privileged(), Ok(rustix::process::geteuid().is_root()));
    }
}
