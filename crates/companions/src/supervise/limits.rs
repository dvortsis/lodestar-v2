//! Companion resource limits and the Q3 (b) sandbox (plan
//! `docs/plan/2026-11-06-v06-companions.md` §4 CMP-4b, §5 Q3 as decided
//! by `docs/owner-decisions.md` §2.47; architecture §18.10).
//!
//! [`Sandbox`] is what CMP-5 holds: one per [`Supervisor`](super::Supervisor).
//! - [`Sandbox::hook`] is the supervisor's [`AfterSpawn`] hook.
//! - [`Sandbox::prepare`] turns a companion's [`SpawnSpec`] into the one the
//!   supervisor starts, and says what will hold ([`Report`]). CMP-5 and
//!   CMP-6 show the report; the install confirmation uses
//!   [`Report::sandbox`] to say plainly when the companion could read
//!   Lodestar's data.
//!
//! **Linux** ([`super::limits_linux`]): the companion is started through
//! Lodestar's own binary as a small launcher ([`launcher_main`]), which
//! sets its rlimits, applies the Landlock ruleset to itself and then
//! `exec`s the companion. Nothing
//! the companion runs is ever outside them. The hook waits for that
//! `exec`, so CMP-4a's record names the companion, not the launcher.
//!
//! **Windows** ([`super::limits_windows`]): every companion is assigned to
//! one Job Object with kill-on-job-close, so companions die with Lodestar.
//! There's no safe restricted-token or AppContainer wrapper, so the
//! sandbox is reported as not applied.
//!
//! Every error is a code: no paths, env values or routes' credentials.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(not(any(windows, target_os = "linux")))]
use super::process::NoHook;
use super::process::{AfterSpawn, SpawnSpec};

/// The open-files limit a companion gets by default: qBittorrent keeps a
/// socket per peer and a handle per open file, and 4096 is several times
/// its default connection limit (500), while staying well under the
/// shipped unit's `LimitNOFILE=8192` for Lodestar as a whole.
pub const DEFAULT_OPEN_FILES: u64 = 4096;

/// The resource limits every companion of one [`Sandbox`] gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// `RLIMIT_NOFILE` on Linux. Held to Lodestar's own hard limit.
    pub open_files: u64,
    /// A memory cap in bytes: `RLIMIT_AS` on Linux. `None` (the default):
    /// no cap, because
    /// .NET (Prowlarr) and Chromium reserve far more address space than
    /// they use and an address-space cap breaks them. Windows has no safe
    /// way to set a job memory cap, so it reports it as not applied.
    pub memory: Option<u64>,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            open_files: DEFAULT_OPEN_FILES,
            memory: None,
        }
    }
}

/// What a companion may touch, besides what every program needs (system
/// libraries, resolver files, `/dev/null`, `/dev/urandom`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confinement {
    /// The installed version's folder (`<data>/companions/<id>/<version>`):
    /// read and execute.
    pub install: PathBuf,
    /// Where it saves downloads: read and write. It may not be Lodestar's
    /// data folder, hold it, or lie inside it outside the companion's own
    /// folder.
    pub downloads: Option<PathBuf>,
}

/// How much of the Q3 sandbox holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Lodestar's data is out of reach.
    Full,
    /// Reads and writes outside its folders are refused, but something
    /// less is not covered (the reason says what).
    Partial,
    /// The companion could read Lodestar's data.
    None,
}

impl Level {
    pub fn code(&self) -> &'static str {
        match self {
            Level::Full => "full",
            Level::Partial => "partial",
            Level::None => "none",
        }
    }
}

/// The sandbox's result for one companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sandboxed {
    pub level: Level,
    /// Why, as a stable code:
    /// - `landlock` (`full`): Landlock ABI 9 or later, which also mediates
    ///   connecting to Unix sockets by path;
    /// - `landlock_unix_sockets` (`partial`, ABI 6 to 8): files are
    ///   confined, but a Unix socket it can reach by path isn't (a user's
    ///   session bus, `systemd --user`, `docker.sock`), and one of those
    ///   could run code outside the sandbox. Not reachable under the
    ///   shipped systemd unit;
    /// - `landlock_no_scopes` (`partial`, ABI 3 to 5): also no signal or
    ///   abstract-socket scoping;
    /// - `landlock_old_abi` (`partial`, ABI 1 or 2): also no truncation
    ///   control, so a file it can't open may still be truncated by name;
    /// - `landlock_unavailable`: the kernel has no Landlock, or it's off;
    /// - `no_safe_wrapper`: Windows, where no safe restricted-token or
    ///   AppContainer wrapper exists;
    /// - `unsupported_platform`.
    pub reason: &'static str,
}

/// How the resource limits are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// Linux rlimits, set before the companion runs.
    Rlimits,
    /// A Windows Job Object.
    JobObject,
    None,
}

impl Mechanism {
    pub fn code(&self) -> &'static str {
        match self {
            Mechanism::Rlimits => "rlimits",
            Mechanism::JobObject => "job_object",
            Mechanism::None => "none",
        }
    }
}

/// The resource limits one companion gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub mechanism: Mechanism,
    /// Its open-files limit, when one is set.
    pub open_files: Option<u64>,
    /// Its memory cap in bytes, when one is set.
    pub memory: Option<u64>,
    /// It dies when Lodestar does, whatever happens to Lodestar (the Job
    /// Object). On Linux CMP-4a's process group and the init's hook do it.
    pub dies_with_lodestar: bool,
    /// Why no cgroup is used (Linux): always `not_delegated` in this
    /// version, since no shipped unit delegates one. `None` off Linux.
    pub cgroup_skipped: Option<&'static str>,
}

/// What [`Sandbox::prepare`] reports for one companion. CMP-5 keeps it with
/// the companion; it holds once the companion is running (a launcher that
/// can't apply it exits, and the start fails).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub sandbox: Sandboxed,
    pub limits: Applied,
}

/// Why a companion couldn't be prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitsError {
    /// A folder isn't an absolute path to an existing directory.
    BadFolder,
    /// The downloads folder is Lodestar's data folder, holds it, or lies
    /// inside it outside the companion's own folder.
    DownloadsOverlapData,
    /// The install folder isn't inside the companion's own folder.
    InstallOutsideCompanion,
    /// The downloads folder is the companion's own folder, or lies inside
    /// its home or install folder, where it could swap a granted folder.
    DownloadsInsideCompanion,
    /// The executable isn't a regular file inside the install folder.
    ExecutableOutsideInstall,
    /// Lodestar's own executable, the launcher, couldn't be found.
    NoLauncher,
    /// The Windows Job Object couldn't be created.
    NoJob,
}

impl LimitsError {
    pub fn code(&self) -> &'static str {
        match self {
            LimitsError::BadFolder => "bad_folder",
            LimitsError::DownloadsOverlapData => "downloads_overlap_data",
            LimitsError::InstallOutsideCompanion => "install_outside_companion",
            LimitsError::DownloadsInsideCompanion => "downloads_inside_companion",
            LimitsError::ExecutableOutsideInstall => "executable_outside_install",
            LimitsError::NoLauncher => "no_launcher",
            LimitsError::NoJob => "no_job",
        }
    }
}

impl fmt::Display for LimitsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "companion limits: {}", self.code())
    }
}

impl std::error::Error for LimitsError {}

/// The program that starts a companion inside its limits (Linux): an
/// executable and the arguments that make it act as the launcher.
/// [`Launcher::lodestar`] is the shipped one; tests use their own binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    pub exe: PathBuf,
    pub args: Vec<std::ffi::OsString>,
}

impl Launcher {
    /// Lodestar's own binary, run as `lodestar <marker>`. Its `main` must
    /// call [`launcher_main`] first (CMP-5 wires it).
    pub fn lodestar() -> Result<Launcher, LimitsError> {
        let exe = std::env::current_exe().map_err(|_| LimitsError::NoLauncher)?;
        Ok(Launcher {
            exe,
            args: vec![LAUNCHER_ARG.into()],
        })
    }
}

/// The argument that makes Lodestar's binary the launcher.
pub const LAUNCHER_ARG: &str = "__lodestar-companion-launcher";

/// Call first thing in Lodestar's `main`: when this process was started as
/// the companion launcher, it applies the limits and sandbox and `exec`s
/// the companion, so it only returns on failure, with the exit code to
/// exit with. Otherwise `None` at once. Off Linux, always `None`.
pub fn launcher_main() -> Option<i32> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(LAUNCHER_ARG)) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        Some(super::limits_linux::run_launcher())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Some(64)
    }
}

/// One supervisor's limits: the hook it's built with, and the
/// per-companion preparation.
pub struct Sandbox {
    limits: Limits,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    launcher: Launcher,
    hook: Arc<dyn AfterSpawn>,
    #[cfg(windows)]
    job: Arc<super::limits_windows::JobHook>,
}

impl fmt::Debug for Sandbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sandbox")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Sandbox {
    /// On Windows this creates the Job Object, which lives as long as the
    /// sandbox and its hook: dropping both kills every companion in it.
    pub fn new(limits: Limits, launcher: Launcher) -> Result<Sandbox, LimitsError> {
        #[cfg(windows)]
        {
            let job = Arc::new(super::limits_windows::JobHook::new()?);
            Ok(Sandbox {
                limits,
                launcher,
                hook: job.clone(),
                job,
            })
        }
        #[cfg(target_os = "linux")]
        {
            let hook = Arc::new(super::limits_linux::ExecHook::new(&launcher.exe));
            Ok(Sandbox {
                limits,
                launcher,
                hook,
            })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Ok(Sandbox {
                limits,
                launcher,
                hook: Arc::new(NoHook),
            })
        }
    }

    /// The supervisor's [`AfterSpawn`] hook.
    pub fn hook(&self) -> Arc<dyn AfterSpawn> {
        self.hook.clone()
    }

    /// The pids in the Job Object (Windows).
    #[cfg(windows)]
    pub fn job_members(&self) -> Vec<u32> {
        self.job.members()
    }

    /// The spec the supervisor starts for `spec`, and what will hold.
    pub fn prepare(
        &self,
        spec: SpawnSpec,
        conf: &Confinement,
    ) -> Result<(SpawnSpec, Report), LimitsError> {
        let folders = check(&spec, conf)?;
        #[cfg(target_os = "linux")]
        {
            super::limits_linux::prepare(spec, &folders, &self.limits, &self.launcher)
        }
        #[cfg(windows)]
        {
            let _ = folders;
            Ok((spec, super::limits_windows::report(&self.limits)))
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = folders;
            Ok((
                spec,
                Report {
                    sandbox: Sandboxed {
                        level: Level::None,
                        reason: "unsupported_platform",
                    },
                    limits: Applied {
                        mechanism: Mechanism::None,
                        open_files: None,
                        memory: None,
                        dies_with_lodestar: false,
                        cgroup_skipped: None,
                    },
                },
            ))
        }
    }
}

/// The folders one companion may use, canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Folders {
    /// `<data>/companions/<id>/home`: read and write.
    pub home: PathBuf,
    pub install: PathBuf,
    pub downloads: Option<PathBuf>,
    /// Lodestar's data folder: nothing may grant access above or to it.
    pub data: PathBuf,
}

fn canonical_dir(p: &Path) -> Result<PathBuf, LimitsError> {
    if !p.is_absolute() {
        return Err(LimitsError::BadFolder);
    }
    let c = std::fs::canonicalize(p).map_err(|_| LimitsError::BadFolder)?;
    if c.is_dir() {
        Ok(c)
    } else {
        Err(LimitsError::BadFolder)
    }
}

/// Checks the folders against Lodestar's data folder.
pub(crate) fn check(spec: &SpawnSpec, conf: &Confinement) -> Result<Folders, LimitsError> {
    let data = canonical_dir(&spec.data_dir)?;
    let own = data.join("companions").join(spec.id.as_str());
    let install = canonical_dir(&conf.install)?;
    let home = super::process::home_dir(&data, &spec.id);
    if !install.starts_with(&own) || install == own || install.starts_with(&home) {
        return Err(LimitsError::InstallOutsideCompanion);
    }
    let exe = std::fs::canonicalize(&spec.executable)
        .ok()
        .filter(|e| e.starts_with(&install) && e.is_file());
    if exe.is_none() {
        return Err(LimitsError::ExecutableOutsideInstall);
    }
    let downloads = match &conf.downloads {
        None => None,
        Some(d) => {
            let d = canonical_dir(d)?;
            // Above or at the data folder grants it all; inside it, only
            // the companion's own folder is allowed.
            if data.starts_with(&d) || (d.starts_with(&data) && !d.starts_with(&own)) {
                return Err(LimitsError::DownloadsOverlapData);
            }
            if d == own || d.starts_with(&home) || d.starts_with(&install) {
                return Err(LimitsError::DownloadsInsideCompanion);
            }
            // The same checks on the filesystem itself, so a second mount
            // of a folder above the data folder can't pass for another.
            #[cfg(target_os = "linux")]
            if !super::limits_linux::mount_separate(&d, &data, &own) {
                return Err(LimitsError::DownloadsOverlapData);
            }
            Some(d)
        }
    };
    // CMP-4a's spawn creates `home` (it may not exist yet); the path is
    // made canonical through the data folder.
    Ok(Folders {
        home,
        install,
        downloads,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inputs::CompanionId;

    fn spec(data: &Path) -> SpawnSpec {
        SpawnSpec {
            id: CompanionId::parse("qbittorrent").unwrap(),
            executable: data
                .join("companions")
                .join("qbittorrent")
                .join("5.0.3")
                .join("x"),
            args: Vec::new(),
            env: Vec::new(),
            data_dir: data.to_owned(),
            allow_privileged: true,
        }
    }

    #[test]
    fn folders_may_not_reach_lodestars_data() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let own = data.join("companions").join("qbittorrent");
        let install = own.join("5.0.3");
        let other = data.join("companions").join("prowlarr");
        let outside = tmp.path().join("dl");
        for d in [
            &install,
            &other,
            &outside,
            &own.join("downloads"),
            &own.join("home").join("Downloads"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(install.join("x"), b"").unwrap();
        let s = spec(&data);
        let conf = |install: &Path, dl: Option<&Path>| Confinement {
            install: install.to_owned(),
            downloads: dl.map(Path::to_owned),
        };
        let ok = check(&s, &conf(&install, Some(&outside))).unwrap();
        assert_eq!(ok.downloads, Some(std::fs::canonicalize(&outside).unwrap()));
        assert!(check(&s, &conf(&install, Some(&own.join("downloads")))).is_ok());
        for bad in [&data, tmp.path(), &other, &data.join("companions")] {
            assert_eq!(
                check(&s, &conf(&install, Some(bad))),
                Err(LimitsError::DownloadsOverlapData),
                "{}",
                bad.display()
            );
        }
        for bad in [&own, &own.join("home").join("Downloads"), &install] {
            assert_eq!(
                check(&s, &conf(&install, Some(bad))),
                Err(LimitsError::DownloadsInsideCompanion),
                "{}",
                bad.display()
            );
        }
        let mut elsewhere = s.clone();
        elsewhere.executable = outside.join("x");
        std::fs::write(&elsewhere.executable, b"").unwrap();
        assert_eq!(
            check(&elsewhere, &conf(&install, None)),
            Err(LimitsError::ExecutableOutsideInstall)
        );
        assert_eq!(
            check(&s, &conf(&other, None)),
            Err(LimitsError::InstallOutsideCompanion)
        );
        assert_eq!(
            check(&s, &conf(&own, None)),
            Err(LimitsError::InstallOutsideCompanion)
        );
        assert_eq!(
            check(&s, &conf(Path::new("relative"), None)),
            Err(LimitsError::BadFolder)
        );
    }

    #[test]
    fn codes_are_stable() {
        assert_eq!(Level::Partial.code(), "partial");
        assert_eq!(Mechanism::JobObject.code(), "job_object");
        assert_eq!(
            LimitsError::DownloadsOverlapData.to_string(),
            "companion limits: downloads_overlap_data"
        );
        assert_eq!(Limits::default().open_files, 4096);
        assert_eq!(Limits::default().memory, None);
    }
}
