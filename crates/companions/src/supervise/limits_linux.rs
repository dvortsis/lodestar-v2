//! Linux limits and sandbox (plan §4 CMP-4b, Q3 (b)).
//!
//! **Before the companion runs.** CMP-4a's spawn can't run code in the
//! child between `fork` and `exec` (that needs `pre_exec`, which is
//! `unsafe`). So the supervisor starts Lodestar's own binary as a launcher
//! ([`run_launcher`]), with the plan in one environment variable. The
//! launcher, single-threaded and before it reads anything else:
//! 1. joins a delegated cgroup v2 leaf, when [`prepare`] found one;
//! 2. sets `RLIMIT_NOFILE`, `RLIMIT_CORE` (0: a core dump would hold the
//!    companion's credentials) and `RLIMIT_AS` when a cap is set;
//! 3. applies the Landlock ruleset to itself, at the best ABI the kernel
//!    has, and checks it reached the level [`prepare`] reported;
//! 4. removes the plan from its environment and `exec`s the companion.
//!
//! The ruleset and limits are inherited across `exec` and by everything
//! the companion starts, so no window is left for untrusted code. A
//! launcher that can't do a step exits with a code, and the start fails.
//!
//! [`ExecHook`] waits, bounded, for that `exec` before CMP-4a records the
//! process, so the record names the companion's executable.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use landlock::{
    ABI, Access, AccessFs, BitFlags, LandlockStatus, RestrictionStatus, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, path_beneath_rules,
};
use rustix::process::{Resource, Rlimit};

use super::limits::{
    Applied, Folders, Launcher, Level, Limits, LimitsError, Mechanism, Report, Sandboxed,
};
use super::process::{AfterSpawn, EnvName, SpawnSpec};

/// The environment variable that carries the launcher's plan. The launcher
/// removes it before `exec`.
pub const PLAN_ENV: &str = "LODESTAR_COMPANION_PLAN";

/// The newest file-system access rights asked for. ABI 9's
/// `ResolveUnix` governs connecting to Unix sockets, not files, and is left
/// out so name lookups through local services keep working.
const ABI_ASKED: ABI = ABI::V8;

/// How long [`ExecHook`] waits for the launcher's `exec`.
const EXEC_WAIT: Duration = Duration::from_secs(10);

/// The pids a delegated cgroup leaf allows: a guard against a runaway
/// companion, far above what qBittorrent or Prowlarr start.
const CGROUP_PIDS: u32 = 1024;

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
    pub cgroup: Option<PathBuf>,
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
        if let Some(g) = &self.cgroup {
            put(&mut out, b'g', g.as_os_str().as_bytes());
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
        let path = |v: &[u8]| {
            let p = PathBuf::from(OsStr::from_bytes(v));
            p.is_absolute().then_some(p)
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
                b'g' if plan.cgroup.is_none() => plan.cgroup = Some(path(v)?),
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
        let status = std::thread::spawn(|| restrict(&Plan::default()).ok())
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
    match s.landlock {
        LandlockStatus::Available { effective_abi, .. } if effective_abi >= ABI::V3 => Sandboxed {
            level: Level::Full,
            reason: "landlock",
        },
        LandlockStatus::Available { effective_abi, .. } if effective_abi >= ABI::V1 => Sandboxed {
            level: Level::Partial,
            reason: "landlock_old_abi",
        },
        _ => none,
    }
}

/// Builds and applies the ruleset for `plan` to the calling thread.
fn restrict(plan: &Plan) -> Result<RestrictionStatus, landlock::RulesetError> {
    let all = AccessFs::from_all(ABI_ASKED);
    let read_exec = AccessFs::from_read(ABI_ASKED);
    let read: BitFlags<AccessFs> = AccessFs::ReadFile | AccessFs::ReadDir;
    let write = read | AccessFs::WriteFile;
    Ruleset::default()
        .handle_access(all)?
        .create()?
        .add_rules(path_beneath_rules(&plan.read_write, all))?
        .add_rules(path_beneath_rules(&plan.read_exec, read_exec))?
        .add_rules(path_beneath_rules(&plan.read, read))?
        .add_rules(path_beneath_rules(SYSTEM_WRITE, write))?
        .restrict_self()
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

/// A delegated cgroup v2 leaf for `id`, or why there's none. Delegated
/// means Lodestar runs in a leaf whose parent belongs to its own user and
/// has the memory and pids controllers enabled for its children; the
/// shipped unit (`ProtectControlGroups=yes`, no `Delegate=`) never does.
fn cgroup_leaf(id: &str) -> Result<PathBuf, &'static str> {
    if rustix::process::geteuid().is_root() {
        return Err("root");
    }
    let own = std::fs::read_to_string("/proc/self/cgroup").map_err(|_| "not_cgroup_v2")?;
    let rel = own
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .ok_or("not_cgroup_v2")?;
    let mount = Path::new("/sys/fs/cgroup");
    let parent = mount
        .join(rel.trim_start_matches('/'))
        .parent()
        .map(Path::to_owned)
        .filter(|p| p.starts_with(mount) && p != mount)
        .ok_or("not_delegated")?;
    let owner = std::fs::metadata(&parent)
        .map_err(|_| "not_delegated")?
        .uid();
    if owner != rustix::process::geteuid().as_raw() {
        return Err("not_delegated");
    }
    let enabled = std::fs::read_to_string(parent.join("cgroup.subtree_control"))
        .map_err(|_| "not_delegated")?;
    let words: Vec<&str> = enabled.split_whitespace().collect();
    if !(words.contains(&"memory") && words.contains(&"pids")) {
        return Err("no_controllers");
    }
    Ok(parent.join(format!("lodestar-companion-{id}")))
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
    let cgroup = cgroup_leaf(spec.id.as_str());
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
        cgroup: cgroup.as_ref().ok().cloned(),
        data: f.data.clone(),
        exe: spec.executable.clone(),
        args: spec.args.clone(),
    };
    let report = Report {
        sandbox,
        limits: Applied {
            mechanism: if cgroup.is_ok() {
                Mechanism::RlimitsAndCgroup
            } else {
                Mechanism::Rlimits
            },
            open_files: Some(open_files),
            memory: limits.memory,
            dies_with_lodestar: false,
            cgroup_skipped: cgroup.err(),
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
    Cgroup = 121,
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
            LaunchError::Cgroup => "launcher_cgroup",
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
            LaunchError::Cgroup,
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
        (Ok(e), Ok(root)) => e.starts_with(&root) && e != root,
        _ => false,
    };
    exe.is_absolute() && regular && under
}

/// Acts as the launcher: applies the plan in [`PLAN_ENV`] to this process
/// and `exec`s the companion. Returns only on failure, with the exit
/// code. [`super::limits::launcher_main`] calls it; tests call it from
/// their fake.
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
    if let Some(leaf) = &plan.cgroup {
        join_cgroup(leaf, plan.memory).map_err(|_| LaunchError::Cgroup)?;
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
    let got = restrict(&plan).map_err(|_| LaunchError::Landlock)?;
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

/// Moves this process into `leaf`, creating it with its limits.
fn join_cgroup(leaf: &Path, memory: Option<u64>) -> std::io::Result<()> {
    match std::fs::create_dir(leaf) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let max = memory.map_or_else(|| "max".to_owned(), |m| m.to_string());
    std::fs::write(leaf.join("memory.max"), max)?;
    std::fs::write(leaf.join("pids.max"), CGROUP_PIDS.to_string())?;
    std::fs::write(leaf.join("cgroup.procs"), "0")
}

/// Waits until a just-spawned launcher has `exec`d the companion, so
/// CMP-4a's record names the companion's executable. A child that isn't
/// a launcher passes at once.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExecHook;

impl AfterSpawn for ExecHook {
    fn applied(&self, child: &tokio::process::Child) -> Result<(), &'static str> {
        let pid = child.id().ok_or("launcher_failed")?;
        wait_for_exec(pid, EXEC_WAIT)
    }
}

fn wait_for_exec(pid: u32, wait: Duration) -> Result<(), &'static str> {
    let marker = format!("{PLAN_ENV}=");
    let until = Instant::now() + wait;
    let mut pause = Duration::from_micros(100);
    loop {
        // The launcher's environment holds the plan; the companion's
        // doesn't. Unreadable (mid-exec) counts as not yet.
        if let Ok(env) = std::fs::read(format!("/proc/{pid}/environ"))
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

    fn sample() -> Plan {
        Plan {
            open_files: 4096,
            memory: Some(1 << 30),
            level: Some(Level::Partial),
            read_write: vec!["/data/companions/q/home".into(), "/dl with:colon".into()],
            read_exec: vec!["/usr".into()],
            read: vec!["/etc/hosts".into()],
            cgroup: Some("/sys/fs/cgroup/x/lodestar-companion-q".into()),
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
        match a.level {
            Level::Full => assert_eq!(a.reason, "landlock"),
            Level::Partial => assert_eq!(a.reason, "landlock_old_abi"),
            Level::None => assert_eq!(a.reason, "landlock_unavailable"),
        }
    }

    #[test]
    fn a_child_that_isnt_a_launcher_passes_and_a_dead_one_fails() {
        let me = std::process::id();
        assert_eq!(wait_for_exec(me, Duration::from_secs(10)), Ok(()));
        assert_eq!(
            wait_for_exec(u32::MAX - 1, Duration::from_secs(10)),
            Err("launcher_failed")
        );
    }
}
