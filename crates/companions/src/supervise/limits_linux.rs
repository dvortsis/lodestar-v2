//! Linux limits and sandbox (plan §4 CMP-4b, Q3 (b)).
//!
//! **Before the companion runs.** CMP-4a's spawn can't run code in the
//! child between `fork` and `exec` (that needs `pre_exec`, which is
//! `unsafe`). So the supervisor starts Lodestar's own binary as a launcher
//! ([`run_launcher`]), with the plan in one environment variable. The
//! launcher, single-threaded and before it reads anything else:
//! 1. re-checks the target and what the plan grants;
//! 2. sets `RLIMIT_NOFILE`, `RLIMIT_CORE` (0: a core dump would hold the
//!    companion's credentials) and `RLIMIT_AS` when a cap is set;
//! 3. applies the Landlock ruleset to itself, at the best ABI the kernel
//!    has, and checks it reached the level [`prepare`] reported;
//! 4. removes the plan from its environment and `exec`s the companion.
//!
//! No cgroup is used: none of the units Lodestar ships delegates one, and
//! a cgroup a same-user companion can leave (`clone3(CLONE_INTO_CGROUP)`)
//! would only be best effort anyway.
//!
//! The ruleset and limits are inherited across `exec` and by everything
//! the companion starts, so no window is left for untrusted code. A
//! launcher that can't do a step exits with a code, and the start fails.
//!
//! [`ExecHook`] waits, bounded, for that `exec` before CMP-4a records the
//! process, so the record names the companion's executable.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use std::os::fd::OwnedFd;

use landlock::{
    ABI, Access, AccessFs, BitFlags, LandlockStatus, PathBeneath, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope,
};
use rustix::fs::{CWD, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;
use rustix::process::{Resource, Rlimit};

use super::limits::{
    Applied, Folders, Launcher, Level, Limits, LimitsError, Mechanism, Report, Sandboxed,
};
use super::process::{AfterSpawn, EnvName, SpawnSpec};

/// The environment variable that carries the launcher's plan. The launcher
/// removes it before `exec`.
pub const PLAN_ENV: &str = "LODESTAR_COMPANION_PLAN";

/// The newest file-system access rights asked for, `ResolveUnix` (ABI 9:
/// connecting to a Unix socket by its path) included, so a companion can't
/// reach a user's session services outside the sandbox.
const ABI_ASKED: ABI = ABI::V9;

/// How long [`ExecHook`] waits for the launcher's `exec`.
const EXEC_WAIT: Duration = Duration::from_secs(10);

/// The only Unix sockets a companion may connect to by path besides its
/// own folders': the name-resolution services. Canonicalised when the
/// rules are built (`/var/run` is a link to `/run`).
const SYSTEM_SOCKETS: &[&str] = &["/run/systemd/resolve", "/run/nscd", "/var/run/nscd"];

/// System folders a program needs to load and run: read and execute.
const SYSTEM_EXEC: &[&str] = &["/usr", "/lib", "/lib64", "/lib32", "/bin", "/sbin"];

/// What a program reads besides its own files: the loader cache, name
/// resolution, time zones, CA certificates, its user's name, and the
/// kernel's views a runtime sizes itself by. Read only.
const SYSTEM_READ: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/host.conf",
    "/etc/nsswitch.conf",
    "/etc/gai.conf",
    "/etc/localtime",
    "/etc/ssl",
    "/etc/pki",
    "/etc/ca-certificates",
    "/etc/passwd",
    "/etc/group",
    "/etc/os-release",
    "/proc",
    "/sys/fs/cgroup",
    "/sys/devices/system/cpu",
    "/dev/urandom",
    "/dev/random",
];

/// Devices a program writes to.
const SYSTEM_WRITE: &[&str] = &["/dev/null", "/dev/zero", "/dev/full"];

/// The launcher's plan, built by [`prepare`] and read by [`run_launcher`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Plan {
    pub open_files: u64,
    pub memory: Option<u64>,
    /// The level [`prepare`] reported; the launcher refuses to run the
    /// companion below it.
    pub level: Option<Level>,
    pub read_write: Vec<PathBuf>,
    pub read_exec: Vec<PathBuf>,
    pub read: Vec<PathBuf>,
    /// Lodestar's data folder: the executable must be under its
    /// `companions/`.
    pub data: PathBuf,
    pub exe: PathBuf,
    pub args: Vec<OsString>,
}

// The plan's encoding: `1` then fields, each a tag byte, the value's
// length in decimal, `:` and the value's bytes. Lengths make any path
// safe to carry (an environment value can't hold NUL, and neither can a
// path or an argument).
const FORMAT: &[u8] = b"1";

fn put(out: &mut Vec<u8>, tag: u8, value: &[u8]) {
    out.push(tag);
    out.extend_from_slice(value.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(value);
}

impl Plan {
    pub fn encode(&self) -> OsString {
        let mut out = FORMAT.to_vec();
        put(&mut out, b'n', self.open_files.to_string().as_bytes());
        if let Some(m) = self.memory {
            put(&mut out, b'm', m.to_string().as_bytes());
        }
        if let Some(l) = self.level {
            put(&mut out, b'l', l.code().as_bytes());
        }
        for (tag, paths) in [
            (b'w', &self.read_write),
            (b'x', &self.read_exec),
            (b'r', &self.read),
        ] {
            for p in paths {
                put(&mut out, tag, p.as_os_str().as_bytes());
            }
        }
        put(&mut out, b'd', self.data.as_os_str().as_bytes());
        put(&mut out, b'e', self.exe.as_os_str().as_bytes());
        for a in &self.args {
            put(&mut out, b'a', a.as_bytes());
        }
        OsString::from_vec(out)
    }

    /// Strict: every field once where it must be, absolute paths, nothing
    /// left over.
    pub fn decode(raw: &OsStr) -> Option<Plan> {
        let mut rest = raw.as_bytes().strip_prefix(FORMAT)?;
        let mut plan = Plan::default();
        let (mut open_files, mut exe, mut data) = (None, None, None);
        let number = |v: &[u8]| std::str::from_utf8(v).ok()?.parse::<u64>().ok();
        // Absolute, with no `..`: the launcher's checks compare paths by
        // their text, and it opens them without following links.
        let path = |v: &[u8]| {
            let p = PathBuf::from(OsStr::from_bytes(v));
            let plain = !p
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
            (p.is_absolute() && plain).then_some(p)
        };
        while let Some((&tag, after)) = rest.split_first() {
            let colon = after.iter().position(|b| *b == b':')?;
            let len: usize = std::str::from_utf8(&after[..colon]).ok()?.parse().ok()?;
            let body = &after[colon + 1..];
            if body.len() < len {
                return None;
            }
            let (v, next) = body.split_at(len);
            rest = next;
            match tag {
                b'n' if open_files.is_none() => open_files = Some(number(v)?),
                b'm' if plan.memory.is_none() => plan.memory = Some(number(v)?),
                b'l' if plan.level.is_none() => {
                    plan.level = Some(match v {
                        b"full" => Level::Full,
                        b"partial" => Level::Partial,
                        b"none" => Level::None,
                        _ => return None,
                    })
                }
                b'w' => plan.read_write.push(path(v)?),
                b'x' => plan.read_exec.push(path(v)?),
                b'r' => plan.read.push(path(v)?),
                b'd' if data.is_none() => data = Some(path(v)?),
                b'e' if exe.is_none() => exe = Some(path(v)?),
                b'a' if exe.is_some() => plan.args.push(OsStr::from_bytes(v).to_owned()),
                _ => return None,
            }
        }
        plan.open_files = open_files?;
        plan.exe = exe?;
        plan.data = data?;
        Some(plan)
    }
}

/// The Landlock level this kernel gives the ruleset, worked out once by
/// applying the same handled rights to a throwaway thread (Landlock
/// restricts the calling thread only; the thread then ends).
pub fn landlock_level() -> Sandboxed {
    static LEVEL: OnceLock<Sandboxed> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        let status = std::thread::spawn(|| restrict(Vec::new()).ok())
            .join()
            .ok()
            .flatten();
        status.map_or(
            Sandboxed {
                level: Level::None,
                reason: "landlock_unavailable",
            },
            |s| level_of(&s),
        )
    })
}

/// What a restriction's status means for Q3.
fn level_of(s: &RestrictionStatus) -> Sandboxed {
    let none = Sandboxed {
        level: Level::None,
        reason: "landlock_unavailable",
    };
    if s.ruleset == RulesetStatus::NotEnforced {
        return none;
    }
    let LandlockStatus::Available { effective_abi, .. } = s.landlock else {
        return none;
    };
    let partial = |reason| Sandboxed {
        level: Level::Partial,
        reason,
    };
    match effective_abi {
        a if a >= ABI::V9 => Sandboxed {
            level: Level::Full,
            reason: "landlock",
        },
        // Pathname Unix sockets aren't mediated: a user's session bus,
        // `systemd --user` or `docker.sock` could run code outside it.
        a if a >= ABI::V6 => partial("landlock_unix_sockets"),
        // No signal or abstract-socket scoping either.
        a if a >= ABI::V3 => partial("landlock_no_scopes"),
        // No truncation control either.
        a if a >= ABI::V1 => partial("landlock_old_abi"),
        _ => none,
    }
}

/// Applies the ruleset to the calling thread: every file-system right up
/// to [`ABI_ASKED`] handled, allowed only beneath `rules`, and no signals
/// or abstract Unix sockets out of the sandbox (ABI 6; best effort, like
/// the rest).
fn restrict(rules: Vec<PathBeneath<OwnedFd>>) -> Result<RestrictionStatus, landlock::RulesetError> {
    Ruleset::default()
        .handle_access(AccessFs::from_all(ABI_ASKED))?
        .scope(Scope::Signal | Scope::AbstractUnixSocket)?
        .create()?
        .add_rules(rules.into_iter().map(Ok::<_, landlock::RulesetError>))?
        .restrict_self()
}

/// Opens `p` (`O_PATH`) without following any link on the way, so a
/// granted folder swapped for a link after [`prepare`] can't widen the
/// grant. `None` when it doesn't exist and isn't `required`.
fn beneath(
    p: &Path,
    access: BitFlags<AccessFs>,
    required: bool,
) -> Result<Option<PathBeneath<OwnedFd>>, ()> {
    let flags = OFlags::PATH | OFlags::CLOEXEC;
    match rustix::fs::openat2(CWD, p, flags, Mode::empty(), ResolveFlags::NO_SYMLINKS) {
        Ok(fd) => {
            let st = rustix::fs::fstat(&fd).map_err(|_| ())?;
            let access = if FileType::from_raw_mode(st.st_mode).is_dir() {
                access
            } else {
                access & AccessFs::from_file(ABI_ASKED)
            };
            Ok(Some(PathBeneath::new(fd, access)))
        }
        Err(Errno::NOENT) if !required => Ok(None),
        Err(_) => Err(()),
    }
}

/// The rules for `plan`. Its own folders must exist; a missing system
/// path is skipped.
fn rules(plan: &Plan) -> Result<Vec<PathBeneath<OwnedFd>>, ()> {
    // Its own folders: everything but execute (W^X: neither companion
    // runs programs from them).
    let own = AccessFs::from_all(ABI_ASKED) & !AccessFs::Execute;
    let read: BitFlags<AccessFs> = AccessFs::ReadFile | AccessFs::ReadDir;
    // `>/dev/null` truncates; terminals and the like need ioctls.
    let write = read | AccessFs::WriteFile | AccessFs::Truncate | AccessFs::IoctlDev;
    let mut out = Vec::new();
    let system_write: Vec<PathBuf> = SYSTEM_WRITE.iter().map(PathBuf::from).collect();
    let sockets: Vec<PathBuf> = SYSTEM_SOCKETS
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .collect();
    for (paths, access, required) in [
        (&plan.read_write, own, true),
        (&plan.read_exec, AccessFs::from_read(ABI_ASKED), false),
        (&plan.read, read, false),
        (&system_write, write, false),
        (&sockets, BitFlags::from(AccessFs::ResolveUnix), false),
    ] {
        for p in paths {
            out.extend(beneath(p, access, required)?);
        }
    }
    Ok(out)
}

/// The companion's own folder, `<data>/companions/<id>`, from its
/// executable's path; never the records folder.
fn own_folder(plan: &Plan) -> Option<PathBuf> {
    let companions = plan.data.join("companions");
    let id = plan
        .exe
        .strip_prefix(&companions)
        .ok()?
        .components()
        .next()?;
    let id = match id {
        std::path::Component::Normal(id) if id != "run" => id,
        _ => return None,
    };
    Some(companions.join(id))
}

/// The launcher's own check of what the plan grants: nothing that is or
/// holds the data folder, and inside it only folders inside the
/// companion's own (`<data>/companions/<id>/…`, not the folder itself).
/// Paths have no `..` and are opened without links, so comparing their
/// text is enough.
fn grants_ok(plan: &Plan) -> bool {
    let Some(own) = own_folder(plan) else {
        return false;
    };
    plan.read_write
        .iter()
        .chain(&plan.read_exec)
        .chain(&plan.read)
        .all(|p| {
            !reaches(p, &plan.data)
                && (!p.starts_with(&plan.data) || (p.starts_with(&own) && p != &own))
        })
}

/// Whether granting `p` would reach Lodestar's data folder: `p` is it,
/// or holds it.
fn reaches(p: &Path, data: &Path) -> bool {
    data.starts_with(p)
}

/// Canonical forms of `paths` that exist and don't reach `data`. A
/// folder that holds the data folder (data under `/usr`, say) is left
/// out: the companion may then fail to load, never read Lodestar's data.
fn system(paths: &[&str], data: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = paths
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .filter(|p| !reaches(p, data))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Undoes `/proc/self/mountinfo`'s octal escapes (`\040` for a space).
fn unescape(b: &[u8]) -> PathBuf {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let oct = b
            .get(i + 1..i + 4)
            .filter(|o| b[i] == b'\\' && o.iter().all(|c| (b'0'..=b'7').contains(c)));
        match oct {
            Some(o) => {
                out.push(
                    o.iter()
                        .fold(0u8, |n, c| n.wrapping_mul(8).wrapping_add(c - b'0')),
                );
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(OsString::from_vec(out))
}

/// Where `p` (canonical) is on its filesystem: the device and the path
/// from the filesystem's root, from the mount taken to serve it: the
/// longest mount point above it, the later one at equal depth. A later
/// mount on a parent folder that hides it isn't seen (L-1 in
/// `docs/decisions/2026-11-08-X-cmp4b-mounts.md`).
fn fs_view(mountinfo: &[u8], p: &Path) -> Option<(String, PathBuf)> {
    let mut best: Option<(usize, String, PathBuf, PathBuf)> = None;
    for (dev, root, point) in mounts(mountinfo) {
        if !p.starts_with(&point) {
            continue;
        }
        let depth = point.components().count();
        if best.as_ref().is_none_or(|b| depth >= b.0) {
            best = Some((depth, dev, root, point));
        }
    }
    let (_, dev, root, point) = best?;
    let rest = p.strip_prefix(&point).ok()?;
    Some((dev, root.join(rest)))
}

/// Each mount's device, root and mount point, read as bytes: a mount
/// point needn't be UTF-8 (a disk with a Latin-1 label).
fn mounts(table: &[u8]) -> impl Iterator<Item = (String, PathBuf, PathBuf)> + '_ {
    table.split(|b| *b == b'\n').filter_map(|line| {
        let mut f = line.split(|b| *b == b' ').skip(2);
        let (dev, root, point) = (f.next()?, f.next()?, f.next()?);
        Some((
            String::from_utf8_lossy(dev).into_owned(),
            unescape(root),
            unescape(point),
        ))
    })
}

/// A folder as its filesystem sees it: the device, and the path from the
/// filesystem's root.
type View = (String, PathBuf);

/// The folders a writable grant is checked against, as views.
struct Views {
    data: View,
    own: View,
    home: View,
    install: View,
}

/// What a writable grant, seen as `(dev, path)`, would reach that the
/// path checks refuse: the data folder (or a folder holding it), the
/// data folder outside the companion's own, the companion's own folder
/// itself, or its home or install folder. `in_home` skips the home rule
/// for what lies inside home, which home's own grant covers.
fn clash(v: &Views, dev: &str, p: &Path, in_home: bool) -> Option<LimitsError> {
    let on = |w: &View| w.0 == dev;
    let (data, own) = (&v.data.1, &v.own.1);
    if on(&v.data) && (data.starts_with(p) || (p.starts_with(data) && !p.starts_with(own))) {
        return Some(LimitsError::DownloadsOverlapData);
    }
    let inside = (on(&v.install) && p.starts_with(&v.install.1))
        || (on(&v.own) && p == own)
        || (!in_home && on(&v.home) && p.starts_with(&v.home.1));
    inside.then_some(LimitsError::DownloadsInsideCompanion)
}

/// The path checks again, on the filesystem itself, for each writable
/// grant (downloads, home) and every mount at or below it, which the
/// grant's rule covers too. A bind mount can show one folder at two
/// paths. Fails closed (`mounts_unreadable`): no readable mount table,
/// or a folder it can't place, refuses.
pub(crate) fn mounts_ok(
    downloads: Option<&Path>,
    home: &Path,
    data: &Path,
    own: &Path,
    install: &Path,
) -> Result<(), LimitsError> {
    let table = std::fs::read("/proc/self/mountinfo").map_err(|_| LimitsError::MountsUnreadable)?;
    mounts_ok_in(&table, downloads, home, data, own, install)
}

fn mounts_ok_in(
    table: &[u8],
    downloads: Option<&Path>,
    home: &Path,
    data: &Path,
    own: &Path,
    install: &Path,
) -> Result<(), LimitsError> {
    let view = |p: &Path| fs_view(table, p).ok_or(LimitsError::MountsUnreadable);
    let v = Views {
        data: view(data)?,
        own: view(own)?,
        home: view(home)?,
        install: view(install)?,
    };
    let grants = downloads
        .map(|d| (d, false))
        .into_iter()
        .chain([(home, true)]);
    for (grant, in_home) in grants {
        let (dev, p) = view(grant)?;
        if let Some(e) = clash(&v, &dev, &p, in_home) {
            return Err(e);
        }
        for (dev, root, point) in mounts(table) {
            if point.starts_with(grant)
                && let Some(e) = clash(&v, &dev, &root, in_home)
            {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// The spec that starts `spec` through the launcher, and the report.
pub(crate) fn prepare(
    spec: SpawnSpec,
    f: &Folders,
    limits: &Limits,
    launcher: &Launcher,
) -> Result<(SpawnSpec, Report), LimitsError> {
    let hard = rustix::process::getrlimit(Resource::Nofile).maximum;
    let open_files = hard.map_or(limits.open_files, |h| limits.open_files.min(h));
    let sandbox = landlock_level();
    let mut read_exec = vec![f.install.clone()];
    read_exec.extend(system(SYSTEM_EXEC, &f.data));
    let plan = Plan {
        open_files,
        memory: limits.memory,
        level: Some(sandbox.level),
        read_write: [Some(f.home.clone()), f.downloads.clone()]
            .into_iter()
            .flatten()
            .collect(),
        read_exec,
        read: system(SYSTEM_READ, &f.data),
        data: f.data.clone(),
        exe: f.exe.clone(),
        args: spec.args.clone(),
    };
    let report = Report {
        sandbox,
        limits: Applied {
            mechanism: Mechanism::Rlimits,
            open_files: Some(open_files),
            memory: limits.memory,
            dies_with_lodestar: false,
            cgroup_skipped: Some("not_delegated"),
        },
    };
    let name = EnvName::parse(PLAN_ENV).map_err(|_| LimitsError::NoLauncher)?;
    let mut env: Vec<(EnvName, OsString)> = spec
        .env
        .into_iter()
        .filter(|(k, _)| k.as_str() != PLAN_ENV)
        .collect();
    env.push((name, plan.encode()));
    let wrapped = SpawnSpec {
        executable: launcher.exe.clone(),
        args: launcher.args.clone(),
        env,
        ..spec
    };
    Ok((wrapped, report))
}

/// Why the launcher stopped, as its exit code and a code on stderr (which
/// goes to the companion's log ring).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchError {
    Plan = 120,
    Rlimit = 122,
    Landlock = 123,
    BelowLevel = 124,
    Exec = 125,
    /// The executable isn't a regular file under `<data>/companions/`.
    Target = 126,
}

impl LaunchError {
    fn code(self) -> &'static str {
        match self {
            LaunchError::Plan => "launcher_bad_plan",
            LaunchError::Rlimit => "launcher_rlimit",
            LaunchError::Landlock => "launcher_landlock",
            LaunchError::BelowLevel => "launcher_below_level",
            LaunchError::Exec => "launcher_exec",
            LaunchError::Target => "launcher_bad_target",
        }
    }

    /// The code for a launcher's exit code, so a failed start says why.
    fn from_exit(code: i32) -> &'static str {
        [
            LaunchError::Plan,
            LaunchError::Rlimit,
            LaunchError::Landlock,
            LaunchError::BelowLevel,
            LaunchError::Exec,
            LaunchError::Target,
        ]
        .into_iter()
        .find(|e| *e as i32 == code)
        .map_or("launcher_failed", LaunchError::code)
    }
}

/// CMP-4a's checks again, in the launcher: an absolute path to a regular
/// file (not a link), under `<data>/companions/`, compared canonically.
/// `PATH` is never searched.
fn target_ok(exe: &Path, data: &Path) -> bool {
    let regular = std::fs::symlink_metadata(exe).is_ok_and(|m| m.file_type().is_file());
    let under = match (
        std::fs::canonicalize(exe),
        std::fs::canonicalize(data.join("companions")),
    ) {
        (Ok(e), Ok(root)) => e.starts_with(&root) && !e.starts_with(root.join("run")),
        _ => false,
    };
    exe.is_absolute() && regular && under
}

/// Acts as the launcher: applies the plan in [`PLAN_ENV`] to this process
/// and `exec`s the companion. Returns only on failure, with the exit
/// code. [`super::limits::launcher_main`] calls it; tests call it from
/// their fake.
#[doc(hidden)]
pub fn run_launcher() -> i32 {
    let e = match launch() {
        Ok(never) => match never {},
        Err(e) => e,
    };
    eprintln!("lodestar companion launcher: {}", e.code());
    e as i32
}

fn launch() -> Result<std::convert::Infallible, LaunchError> {
    let raw = std::env::var_os(PLAN_ENV).ok_or(LaunchError::Plan)?;
    let plan = Plan::decode(&raw).ok_or(LaunchError::Plan)?;
    if !target_ok(&plan.exe, &plan.data) {
        return Err(LaunchError::Target);
    }
    if !grants_ok(&plan) {
        return Err(LaunchError::Plan);
    }
    let set = |r: Resource, v: u64| {
        rustix::process::setrlimit(
            r,
            Rlimit {
                current: Some(v),
                maximum: Some(v),
            },
        )
    };
    set(Resource::Nofile, plan.open_files).map_err(|_| LaunchError::Rlimit)?;
    set(Resource::Core, 0).map_err(|_| LaunchError::Rlimit)?;
    if let Some(m) = plan.memory {
        set(Resource::As, m).map_err(|_| LaunchError::Rlimit)?;
    }
    // Every granted path is opened now, before the restriction, without
    // following links.
    let got = rules(&plan)
        .map_err(|_| LaunchError::Landlock)
        .and_then(|r| restrict(r).map_err(|_| LaunchError::Landlock))?;
    let rank = |l: Level| match l {
        Level::Full => 2,
        Level::Partial => 1,
        Level::None => 0,
    };
    if rank(level_of(&got).level) < rank(plan.level.unwrap_or(Level::Full)) {
        return Err(LaunchError::BelowLevel);
    }
    let err = std::process::Command::new(&plan.exe)
        .args(&plan.args)
        .env_remove(PLAN_ENV)
        .exec();
    let _ = err;
    Err(LaunchError::Exec)
}

/// Waits until a just-spawned launcher has `exec`d the companion, so
/// CMP-4a's record names the companion's executable. A child that isn't
/// a launcher passes at once.
#[derive(Debug, Clone)]
pub struct ExecHook {
    /// The launcher's executable, canonical.
    launcher: PathBuf,
}

impl ExecHook {
    pub fn new(launcher: &Path) -> ExecHook {
        ExecHook {
            launcher: std::fs::canonicalize(launcher).unwrap_or_else(|_| launcher.to_owned()),
        }
    }
}

impl AfterSpawn for ExecHook {
    fn applied(&self, child: &tokio::process::Child) -> Result<(), &'static str> {
        let pid = child.id().ok_or("launcher_failed")?;
        wait_for_exec(pid, &self.launcher, EXEC_WAIT)
    }
}

fn wait_for_exec(pid: u32, launcher: &Path, wait: Duration) -> Result<(), &'static str> {
    let marker = format!("{PLAN_ENV}=");
    let until = Instant::now() + wait;
    let mut pause = Duration::from_micros(100);
    loop {
        // Exec'd: it's no longer the launcher's executable, and its
        // environment (readable, so not mid-exec) has no plan. Both,
        // because either can be read in the middle of an `exec`.
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
        let not_launcher = exe.is_some_and(|e| {
            let e = e
                .to_str()
                .and_then(|s| s.strip_suffix(" (deleted)"))
                .map_or(e.clone(), PathBuf::from);
            e != launcher
        });
        if not_launcher
            && let Ok(env) = std::fs::read(format!("/proc/{pid}/environ"))
            && !env
                .split(|b| *b == 0)
                .any(|v| v.starts_with(marker.as_bytes()))
        {
            return Ok(());
        }
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let state = stat.rsplit_once(") ").and_then(|(_, s)| s.chars().next());
        if matches!(state, None | Some('Z' | 'X')) {
            return Err(exit_reason(&stat));
        }
        if Instant::now() >= until {
            return Err("launcher_timeout");
        }
        // Blocking is fine: CMP-4a runs spawns on the blocking pool.
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

/// Why an exited launcher stopped: `exit_code` (field 52 of
/// `/proc/<pid>/stat`, a wait status) mapped to its code.
fn exit_reason(stat: &str) -> &'static str {
    let status = stat
        .rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(52 - 3))
        .and_then(|v| v.parse::<i32>().ok());
    match status {
        // Exited normally: the code is the second byte.
        Some(w) if w & 0x7f == 0 => LaunchError::from_exit((w >> 8) & 0xff),
        _ => "launcher_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Escapes the way the kernel writes mountinfo: space, tab, newline
    /// and backslash as `\ooo`.
    fn kernel_escape(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in b {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\\') {
                out.extend_from_slice(format!("\\{c:03o}").as_bytes());
            } else {
                out.push(*c);
            }
        }
        out
    }

    proptest! {
        #[test]
        fn unescape_undoes_the_kernels_escapes(b in proptest::collection::vec(any::<u8>(), 0..64)) {
            prop_assert_eq!(unescape(&kernel_escape(&b)).into_os_string().into_vec(), b);
        }

        #[test]
        fn the_parser_never_panics(
            table in proptest::collection::vec(any::<u8>(), 0..512),
            path in proptest::collection::vec(any::<u8>(), 0..64),
        ) {
            let _ = mounts(&table).count();
            let _ = fs_view(&table, Path::new(OsStr::from_bytes(&path)));
        }
    }

    #[test]
    fn unescape_fixed_cases() {
        let u = |b: &[u8]| unescape(b).into_os_string().into_vec();
        assert_eq!(u(b"\\011"), b"\t");
        assert_eq!(u(b"\\012"), b"\n");
        assert_eq!(u(b"\\134"), b"\\");
        // An escaped backslash followed by digits stays digits.
        assert_eq!(u(b"\\134040"), b"\\040");
        // Cut short at the end of the field, or not octal: kept as is.
        assert_eq!(u(b"a\\01"), b"a\\01");
        assert_eq!(u(b"a\\"), b"a\\");
        assert_eq!(u(b"\\8"), b"\\8");
        assert_eq!(u(b"\\078"), b"\\078");
    }

    fn sample() -> Plan {
        Plan {
            open_files: 4096,
            memory: Some(1 << 30),
            level: Some(Level::Partial),
            read_write: vec!["/data/companions/q/home".into(), "/dl with:colon".into()],
            read_exec: vec!["/usr".into()],
            read: vec!["/etc/hosts".into()],
            data: "/data".into(),
            exe: "/data/companions/q/5/qbittorrent-nox".into(),
            args: vec![
                "--profile=a:b".into(),
                "".into(),
                OsString::from_vec(vec![0xff]),
            ],
        }
    }

    #[test]
    fn plans_round_trip() {
        let p = sample();
        assert_eq!(Plan::decode(&p.encode()), Some(p));
        let bare = Plan {
            open_files: 1,
            exe: "/x".into(),
            data: "/d".into(),
            ..Plan::default()
        };
        assert_eq!(Plan::decode(&bare.encode()), Some(bare));
    }

    #[test]
    fn bad_plans_are_refused() {
        let good = sample().encode().into_vec();
        for bad in [
            &b""[..],
            b"2n1:1d2:/de2:/x",
            b"1n1:1d2:/d",             // no executable
            b"1d2:/de2:/x",            // no open-files limit
            b"1n1:1e2:/x",             // no data folder
            b"1n1:1n1:2d2:/de2:/x",    // twice
            b"1n1:1d2:/de1:x",         // relative
            b"1n1:1d2:/da1:ye2:/x",    // an argument before the executable
            b"1n1:1d2:/de2:/x?",       // trailing bytes
            b"1n9:1d2:/de2:/x",        // a length past the end
            b"1n1:1l4:mostd2:/de2:/x", // an unknown level
            b"1n2:-1d2:/de2:/x",
        ] {
            assert_eq!(Plan::decode(OsStr::from_bytes(bad)), None, "{bad:?}");
        }
        // A cut inside a field never decodes: a prefix that does ends
        // on a field and is exactly what it decoded to.
        for cut in 1..good.len() {
            if let Some(p) = Plan::decode(OsStr::from_bytes(&good[..cut])) {
                assert_eq!(p.encode().as_bytes(), &good[..cut]);
            }
        }
    }

    #[test]
    fn the_minimal_plan_decodes() {
        assert!(Plan::decode(OsStr::from_bytes(b"1n1:1d2:/de2:/x")).is_some());
    }

    #[test]
    fn a_launchers_exit_code_names_its_reason() {
        // Fields 3..=52 after the name; exit_code (52) last.
        let stat = |w: i32| {
            let mut f = vec!["0"; 49];
            f[0] = "Z";
            let w = w.to_string();
            format!("7 (a) b) {} {w}", f.join(" "))
        };
        assert_eq!(exit_reason(&stat(124 << 8)), "launcher_below_level");
        assert_eq!(exit_reason(&stat(126 << 8)), "launcher_bad_target");
        assert_eq!(exit_reason(&stat(3 << 8)), "launcher_failed");
        assert_eq!(exit_reason(&stat(9)), "launcher_failed"); // killed
        assert_eq!(exit_reason("garbage"), "launcher_failed");
    }

    #[test]
    fn the_target_must_be_a_file_under_companions() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let dir = data.join("companions").join("q").join("1");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("bin");
        std::fs::write(&exe, b"").unwrap();
        let outside = tmp.path().join("bin");
        std::fs::write(&outside, b"").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(target_ok(&exe, &data));
        assert!(!target_ok(&outside, &data));
        assert!(!target_ok(&link, &data));
        assert!(!target_ok(&dir, &data));
        assert!(!target_ok(Path::new("bin"), &data));
    }

    #[test]
    fn grants_inside_the_data_folder_stay_in_the_companions_own() {
        let plan = |w: &str| Plan {
            open_files: 1,
            read_write: vec![w.into()],
            data: "/d".into(),
            exe: "/d/companions/q/1/bin".into(),
            ..Plan::default()
        };
        assert!(grants_ok(&plan("/d/companions/q/home")));
        assert!(grants_ok(&plan("/srv/downloads")));
        for bad in [
            "/d/companions/q",
            "/d/companions",
            "/d/companions/run",
            "/d/companions/p/home",
            "/d/secrets.key",
            "/d",
            "/",
        ] {
            assert!(!grants_ok(&plan(bad)), "{bad}");
        }
        let mut run = plan("/srv/x");
        run.exe = "/d/companions/run/bin".into();
        assert!(!grants_ok(&run));
    }

    #[test]
    fn second_mounts_are_caught_on_the_filesystem_itself() {
        // `/` is /dev/sda1. /mnt/alias is a bind mount of /var/lib on it;
        // /mnt/my dl a bind of /srv/dl; /media/usb another device.
        let base = "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n\
                    2 1 8:1 /var/lib /mnt/alias rw - ext4 /dev/sda1 rw\n\
                    3 1 8:1 /srv/dl /mnt/my\\040dl rw - ext4 /dev/sda1 rw\n\
                    4 1 8:17 / /media/usb rw - ext4 /dev/sdb1 rw\n";
        let own = Path::new("/var/lib/lodestar/companions/q");
        let check = |table: &str, dl: &str| {
            mounts_ok_in(
                table.as_bytes(),
                Some(Path::new(dl)),
                &own.join("home"),
                Path::new("/var/lib/lodestar"),
                own,
                &own.join("5.0.3"),
            )
        };
        let overlap = Err(LimitsError::DownloadsOverlapData);
        let inside = Err(LimitsError::DownloadsInsideCompanion);
        // An alias above the data folder, of it, or of another companion.
        assert_eq!(check(base, "/mnt/alias"), overlap);
        assert_eq!(check(base, "/mnt/alias/lodestar"), overlap);
        assert_eq!(check(base, "/mnt/alias/lodestar/companions/p"), overlap);
        // (c) An alias of its own folder, home or install folder.
        assert_eq!(check(base, "/mnt/alias/lodestar/companions/q"), inside);
        assert_eq!(
            check(base, "/mnt/alias/lodestar/companions/q/home/dl"),
            inside
        );
        assert_eq!(
            check(base, "/mnt/alias/lodestar/companions/q/5.0.3"),
            inside
        );
        // Its downloads folder through the alias, another device, plain
        // folders.
        assert_eq!(
            check(base, "/mnt/alias/lodestar/companions/q/downloads"),
            Ok(())
        );
        assert_eq!(check(base, "/media/usb/dl"), Ok(()));
        assert_eq!(check(base, "/srv/dl"), Ok(()));
        assert_eq!(check(base, "/mnt/my dl"), Ok(()));
        assert_eq!(
            fs_view(base.as_bytes(), Path::new("/mnt/my dl/x")),
            Some(("8:1".to_owned(), PathBuf::from("/srv/dl/x")))
        );
        // (b) A bind of the data folder below the downloads folder, or
        // below home.
        let below = format!("{base}5 1 8:1 /var/lib/lodestar /srv/dl/sub rw - ext4 /dev/sda1 rw\n");
        assert_eq!(check(&below, "/srv/dl"), overlap);
        let under_home = format!(
            "{base}6 1 8:1 /var/lib /var/lib/lodestar/companions/q/home/x rw - ext4 /dev/sda1 rw\n"
        );
        assert_eq!(check(&under_home, "/srv/dl"), overlap);
        // An alias of its own folder mounted below home (W^X).
        let own_under_home = format!(
            "{base}7 1 8:1 /var/lib/lodestar/companions/q /var/lib/lodestar/companions/q/home/o rw - ext4 /dev/sda1 rw\n"
        );
        assert_eq!(check(&own_under_home, "/srv/dl"), inside);
        // Home itself a bind of the companion's own folder.
        let home_is_own = format!(
            "{base}9 1 8:1 /var/lib/lodestar/companions/q /var/lib/lodestar/companions/q/home rw - ext4 /dev/sda1 rw\n"
        );
        assert_eq!(check(&home_is_own, "/srv/dl"), inside);
        // (a) Fails closed, with its own reason: a table that can't place
        // a folder refuses.
        assert_eq!(check("", "/srv/dl"), Err(LimitsError::MountsUnreadable));
        // A mount point that isn't UTF-8 elsewhere in the table changes
        // nothing.
        let mut latin1 = base.as_bytes().to_vec();
        latin1.extend_from_slice(b"8 1 8:33 / /media/caf\xe9 rw - vfat /dev/sdc1 rw\n");
        let own_home = own.join("home");
        let run = |dl: &str| {
            mounts_ok_in(
                &latin1,
                Some(Path::new(dl)),
                &own_home,
                Path::new("/var/lib/lodestar"),
                own,
                &own.join("5.0.3"),
            )
        };
        assert_eq!(run("/srv/dl"), Ok(()));
        // A refusal through a mount point that isn't UTF-8: the data
        // folder bound below it.
        let mut bound = latin1.clone();
        bound.extend_from_slice(
            b"10 8 8:1 /var/lib/lodestar /media/caf\xe9/x rw - ext4 /dev/sda1 rw\n",
        );
        assert_eq!(
            mounts_ok_in(
                &bound,
                Some(Path::new(OsStr::from_bytes(b"/media/caf\xe9"))),
                &own_home,
                Path::new("/var/lib/lodestar"),
                own,
                &own.join("5.0.3"),
            ),
            overlap
        );
        // A downloads folder on that disk, end to end.
        assert_eq!(
            mounts_ok_in(
                &latin1,
                Some(Path::new(OsStr::from_bytes(b"/media/caf\xe9/dl"))),
                &own_home,
                Path::new("/var/lib/lodestar"),
                own,
                &own.join("5.0.3"),
            ),
            Ok(())
        );
        assert_eq!(run("/mnt/alias"), overlap);
        assert_eq!(
            fs_view(&latin1, Path::new(OsStr::from_bytes(b"/media/caf\xe9/x"))),
            Some(("8:33".to_owned(), PathBuf::from("/x")))
        );
    }

    #[test]
    fn system_paths_never_reach_the_data_folder() {
        let got = system(
            &["/usr", "/etc/hosts", "/does-not-exist"],
            Path::new("/usr/local/x"),
        );
        assert!(!got.iter().any(|p| p == Path::new("/usr")));
        assert!(reaches(Path::new("/"), Path::new("/var/lib/lodestar")));
        assert!(reaches(
            Path::new("/var/lib/lodestar"),
            Path::new("/var/lib/lodestar")
        ));
        assert!(!reaches(
            Path::new("/var/lib/lodestar/companions/q/home"),
            Path::new("/var/lib/lodestar")
        ));
    }

    #[test]
    fn the_level_is_worked_out_once_and_is_consistent() {
        let a = landlock_level();
        assert_eq!(a, landlock_level());
        let ok = match a.level {
            Level::Full => a.reason == "landlock",
            Level::Partial => matches!(
                a.reason,
                "landlock_unix_sockets" | "landlock_no_scopes" | "landlock_old_abi"
            ),
            Level::None => a.reason == "landlock_unavailable",
        };
        assert!(ok, "{a:?}");
    }

    #[test]
    fn a_child_that_isnt_a_launcher_passes_and_a_dead_one_fails() {
        let me = std::process::id();
        let other = Path::new("/nonexistent/launcher");
        assert_eq!(wait_for_exec(me, other, Duration::from_secs(10)), Ok(()));
        assert_eq!(
            wait_for_exec(u32::MAX - 1, other, Duration::from_secs(10)),
            Err("launcher_failed")
        );
        // Still the launcher: waits, then gives up.
        let mine = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(
            wait_for_exec(me, &mine, Duration::from_millis(50)),
            Err("launcher_timeout")
        );
    }
}
