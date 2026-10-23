//! PUID/PGID: dropping root at start-up (architecture §11).
//!
//! The distroless image has no shell, so there's no entrypoint script to
//! switch users; the binary does it itself. Started as root on Linux, it
//! takes `PUID`/`PGID` (default 65532, distroless `nonroot`), makes the data
//! directory theirs, then sets the supplementary groups, gid and uid, in that
//! order, and checks that root can't be regained. It runs first in `main`:
//! before the config file is read, any socket is bound or any thread is
//! spawned. Started as anyone else, it changes nothing.
//!
//! The ids are set with the raw per-thread syscalls (the workspace forbids
//! `unsafe`, so libc's process-wide wrappers are out). That is the whole
//! process only while it has one thread, so the drop refuses to run unless
//! `/proc/self/task` holds exactly one; threads spawned later inherit the
//! dropped ids.
// Most of this is the Linux drop; elsewhere only the tests use it.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// The image's default account: distroless `nonroot`.
pub const DEFAULT_ID: u32 = 65532;

/// The ids to switch to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ids {
    pub uid: u32,
    pub gid: u32,
}

/// What the drop prepares before switching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `serve`, `import-bitmagnet` and the maintenance commands that write
    /// in the data directory (`backup`, `rollback`, `vacuum`, `reindex`):
    /// the data directory is made the target account's first.
    DataDir,
    /// `healthcheck` and `stats`: switch only; they never write.
    ProcessOnly,
}

/// What was done, logged once logging is up (the config, which sets the
/// log filter, is read only after the drop).
#[derive(Debug, Default)]
pub struct Report {
    info: Vec<String>,
    warn: Vec<String>,
    /// The directory whose ownership was checked, if any.
    data_dir: Option<PathBuf>,
}

impl Report {
    pub fn log(&self, data_dir: &Path) {
        for line in &self.info {
            tracing::info!("{line}");
        }
        for line in &self.warn {
            tracing::warn!("{line}");
        }
        if let Some(checked) = &self.data_dir
            && checked != data_dir
        {
            tracing::warn!(
                "the data directory is {} (from the config file), but PUID/PGID ownership \
                 was checked on {}; set LODESTAR_DATA_DIR so both agree",
                data_dir.display(),
                checked.display()
            );
        }
    }

    /// Not started as root: `PUID`/`PGID` can't apply. One line if they ask
    /// for ids other than the current ones.
    fn unprivileged(puid: Option<&str>, pgid: Option<&str>, uid: u32, gid: u32) -> Report {
        let differs = |v: Option<&str>, current: u32| {
            v.is_some_and(|v| !v.is_empty() && v.parse::<u32>().ok() != Some(current))
        };
        let mut report = Report::default();
        if differs(puid, uid) || differs(pgid, gid) {
            report.info.push(format!(
                "PUID/PGID are ignored: running as {uid}:{gid}, not root, so the ids can't \
                 change (with docker --user, the data directory must already be writable \
                 by that account)"
            ));
        }
        report
    }
}

/// Parses and validates `PUID`/`PGID`. Unset or empty means [`DEFAULT_ID`];
/// 0 (root) needs `allow_root`.
pub fn parse_ids(puid: Option<&str>, pgid: Option<&str>, allow_root: bool) -> Result<Ids, String> {
    Ok(Ids {
        uid: parse_id("PUID", puid, allow_root)?,
        gid: parse_id("PGID", pgid, allow_root)?,
    })
}

fn parse_id(name: &str, value: Option<&str>, allow_root: bool) -> Result<u32, String> {
    let Some(v) = value.filter(|v| !v.is_empty()) else {
        return Ok(DEFAULT_ID);
    };
    // Digits only: no sign, space or hex, which `u32::from_str` would
    // partly accept.
    if !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{name}={v:?} is not a number"));
    }
    let id: u32 = v
        .parse()
        .map_err(|_| format!("{name}={v:?} is out of range"))?;
    if id == u32::MAX {
        // (uid_t)-1 means "leave unchanged" to the set*id calls.
        return Err(format!("{name}={id} is reserved"));
    }
    if id == 0 && !allow_root {
        return Err(format!(
            "{name}=0 would keep root; set LODESTAR_ALLOW_ROOT=1 if you really mean it"
        ));
    }
    Ok(id)
}

/// The data directory as the config will see it from the environment.
/// `config.toml` can't be read yet: it lives in the directory, and nothing
/// under it is opened as root. [`Report::log`] warns if the file moves it.
fn env_data_dir(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    env("LODESTAR_DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from)
}

/// Drops root if the process has it. Any error must abort start-up.
pub fn run(scope: Scope, env: impl Fn(&str) -> Option<String>) -> Result<Report, String> {
    imp::run(scope, &env)
}

/// Reads a `Key:\tvalue` line of `/proc/<pid>/status`.
fn status_field<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':').map(|v| v.trim()))
}

#[cfg(target_os = "linux")]
mod imp {
    use std::ffi::CStr;
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::Path;

    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
    use rustix::process::{Gid, Uid, getegid, geteuid, getgid, getgroups, getuid};
    use rustix::thread::{
        set_thread_gid, set_thread_groups, set_thread_res_gid, set_thread_res_uid, set_thread_uid,
    };

    use super::{Ids, Report, Scope, env_data_dir, parse_ids, status_field};

    /// Deeper than anything Lodestar writes; past it, nothing is changed.
    const MAX_DEPTH: usize = 32;

    pub(super) fn run(
        scope: Scope,
        env: &impl Fn(&str) -> Option<String>,
    ) -> Result<Report, String> {
        let (puid, pgid) = (env("PUID"), env("PGID"));
        if !geteuid().is_root() {
            return Ok(Report::unprivileged(
                puid.as_deref(),
                pgid.as_deref(),
                getuid().as_raw(),
                getgid().as_raw(),
            ));
        }
        let allow_root = env("LODESTAR_ALLOW_ROOT").as_deref() == Some("1");
        let ids = parse_ids(puid.as_deref(), pgid.as_deref(), allow_root)?;
        single_thread()?;
        if ids.uid != 0 {
            // Before anything is changed: a drop that can't happen mustn't
            // leave the data directory handed over.
            can_drop(&proc_status()?)?;
        }
        let mut report = Report::default();
        if scope == Scope::DataDir {
            let dir = env_data_dir(env);
            report.info.push(chown_data_dir(&dir, ids)?);
            report.data_dir = Some(dir);
        }
        if ids.uid == 0 {
            report.warn.push(format!(
                "running as root (PUID=0 with LODESTAR_ALLOW_ROOT=1, PGID={}): privileges \
                 were not dropped",
                ids.gid
            ));
            return Ok(report);
        }
        drop_to(ids)?;
        if let Some(dir) = &report.data_dir {
            reachable(dir, ids)?;
            report.info.push(format!(
                "dropped root: running as {}:{} with no supplementary groups",
                ids.uid, ids.gid
            ));
        }
        Ok(report)
    }

    /// CAP_SETGID (6) and CAP_SETUID (7): what setgroups and setres*id need.
    /// Root can lack them (compose `cap_drop: ALL`).
    pub(super) fn can_drop(status: &str) -> Result<(), String> {
        const NEEDED: u64 = 1 << 6 | 1 << 7;
        let eff = status_field(status, "CapEff")
            .and_then(|v| u64::from_str_radix(v, 16).ok())
            .ok_or("/proc/self/status has no readable CapEff")?;
        if eff & NEEDED == NEEDED {
            Ok(())
        } else {
            Err(
                "started as root without the SETUID/SETGID capabilities, so it can't \
                 switch to PUID/PGID: keep those capabilities, or run with \
                 --user <uid>:<gid> instead"
                    .into(),
            )
        }
    }

    /// After the drop, the new account must reach the directory it now
    /// owns; a parent it can't enter is the usual reason it can't.
    fn reachable(dir: &Path, ids: Ids) -> Result<(), String> {
        use rustix::fs::Access;
        rustix::fs::access(dir, Access::READ_OK | Access::WRITE_OK | Access::EXEC_OK).map_err(|e| {
            format!(
                "the data directory {} belongs to {}:{} but that user can't open it \
                     ({e}): a parent folder doesn't let it in. Give the parent folders \
                     execute permission for others (chmod o+x <folder>), or move the data \
                     directory somewhere that user can reach",
                dir.display(),
                ids.uid,
                ids.gid
            )
        })
    }

    fn proc_status() -> Result<String, String> {
        std::fs::read_to_string("/proc/self/status")
            .map_err(|e| format!("reading /proc/self/status: {e}"))
    }

    /// The set*id syscalls below change one thread; the drop is only
    /// process-wide while there is no other.
    fn single_thread() -> Result<(), String> {
        let tasks = std::fs::read_dir("/proc/self/task")
            .map_err(|e| format!("reading /proc/self/task: {e}"))?
            .count();
        if tasks == 1 {
            Ok(())
        } else {
            Err(format!(
                "{tasks} threads are running; privileges must be dropped before any start"
            ))
        }
    }

    pub(super) fn drop_to(ids: Ids) -> Result<(), String> {
        let (uid, gid) = (Uid::from_raw(ids.uid), Gid::from_raw(ids.gid));
        set_thread_groups(&[]).map_err(|e| format!("setgroups([]): {e}"))?;
        set_thread_res_gid(gid, gid, gid).map_err(|e| format!("setgid({}): {e}", ids.gid))?;
        set_thread_res_uid(uid, uid, uid).map_err(|e| format!("setuid({}): {e}", ids.uid))?;
        verify(ids, &proc_status()?)?;
        // Nothing exec'd later can gain privileges through set-id bits or
        // file capabilities. Threads spawned from here inherit it.
        rustix::thread::set_no_new_privs(true).map_err(|e| format!("no_new_privs: {e}"))
    }

    /// The drop happened and can't be undone.
    fn verify(ids: Ids, status: &str) -> Result<(), String> {
        let (uid, gid) = (Uid::from_raw(ids.uid), Gid::from_raw(ids.gid));
        if getuid() != uid || geteuid() != uid || getgid() != gid || getegid() != gid {
            return Err(format!(
                "ids after the drop are {}/{}:{}/{}, not {}:{}",
                getuid().as_raw(),
                geteuid().as_raw(),
                getgid().as_raw(),
                getegid().as_raw(),
                ids.uid,
                ids.gid
            ));
        }
        let groups = getgroups().map_err(|e| format!("getgroups: {e}"))?;
        if !groups.is_empty() {
            return Err(format!("{} supplementary groups remain", groups.len()));
        }
        if set_thread_uid(Uid::ROOT).is_ok() || set_thread_res_uid(None, Uid::ROOT, None).is_ok() {
            return Err("setuid(0) still succeeds after the drop".into());
        }
        if ids.gid != 0 && set_thread_gid(Gid::ROOT).is_ok() {
            return Err("setgid(0) still succeeds after the drop".into());
        }
        check_no_caps(status)
    }

    /// No capability survives in the permitted, effective or ambient sets.
    pub(super) fn check_no_caps(status: &str) -> Result<(), String> {
        for key in ["CapPrm", "CapEff", "CapAmb"] {
            match status_field(status, key) {
                Some(v) if !v.is_empty() && v.bytes().all(|b| b == b'0') => {}
                Some(v) => return Err(format!("{key} is {v} after the drop")),
                // CapAmb is missing before Linux 4.3; there's no ambient set.
                None if key == "CapAmb" => {}
                None => return Err(format!("/proc/self/status has no {key}")),
            }
        }
        Ok(())
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    pub(super) struct Walk {
        pub changed: u64,
        /// Regular files with other hard links: one could be a file outside
        /// the directory.
        pub hard_links: u64,
        /// Devices, fifos and sockets.
        pub special: u64,
        pub too_deep: u64,
    }

    /// Makes `dir` (created if missing) the target account's. The tree is
    /// walked only if the directory itself has another owner, so a large
    /// index isn't rescanned on every start.
    pub(super) fn chown_data_dir(dir: &Path, ids: Ids) -> Result<String, String> {
        let shown = dir.display();
        match std::fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!(
                    "the data directory {shown} is a symlink; set LODESTAR_DATA_DIR to the \
                     directory it points to"
                ));
            }
            Ok(m) if !m.is_dir() => return Err(format!("{shown} is not a directory")),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(dir).map_err(|e| format!("creating {shown}: {e}"))?;
            }
            Err(e) => return Err(format!("{shown}: {e}")),
        }
        // O_NOFOLLOW: a symlink swapped in since the check fails here.
        let fd = rustix::fs::open(
            dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| format!("opening {shown}: {e}"))?;
        let st = rustix::fs::fstat(&fd).map_err(|e| format!("{shown}: {e}"))?;
        if (st.st_uid, st.st_gid) == (ids.uid, ids.gid) {
            return Ok(format!(
                "data directory {shown} is owned by {}:{}",
                ids.uid, ids.gid
            ));
        }
        let root = rustix::fs::stat("/").map_err(|e| format!("stat /: {e}"))?;
        if (st.st_dev, st.st_ino) == (root.st_dev, root.st_ino) {
            return Err(format!(
                "the data directory {shown} is the filesystem root; refusing to chown it"
            ));
        }
        looks_like_data(fd.as_fd(), dir)?;
        // The directory itself last: a walk that fails part-way is retried
        // on the next start, which only walks while its owner differs.
        let mut walk = Walk::default();
        chown_tree(fd.as_fd(), dir, ids, 0, &mut walk)?;
        rustix::fs::fchown(
            &fd,
            Some(Uid::from_raw(ids.uid)),
            Some(Gid::from_raw(ids.gid)),
        )
        .map_err(|e| format!("chown {shown}: {e}"))?;
        walk.changed += 1;
        let mut line = format!(
            "data directory {shown} was owned by {}:{}; changed {} entries to {}:{}",
            st.st_uid, st.st_gid, walk.changed, ids.uid, ids.gid
        );
        for (n, what) in [
            (walk.hard_links, "hard-linked files"),
            (walk.special, "device, fifo or socket entries"),
            (walk.too_deep, "directories nested too deep"),
        ] {
            if n > 0 {
                line.push_str(&format!("; left {n} {what} unchanged"));
            }
        }
        Ok(line)
    }

    /// What Lodestar leaves in a data directory; one is enough.
    const MARKERS: [&CStr; 3] = [c"lodestar.db", c"config.toml", c"index"];

    /// A directory is only handed over whole if it is empty or plainly
    /// Lodestar's, so a mistyped LODESTAR_DATA_DIR (`/srv`, `/var/lib`)
    /// can't give a whole tree away.
    pub(super) fn looks_like_data(
        dir: rustix::fd::BorrowedFd<'_>,
        path: &Path,
    ) -> Result<(), String> {
        let found = |name: &CStr| rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).is_ok();
        if MARKERS.iter().any(|m| found(m)) {
            return Ok(());
        }
        let mut entries = Dir::read_from(dir).map_err(|e| format!("{}: {e}", path.display()))?;
        while let Some(entry) = entries.read() {
            let entry = entry.map_err(|e| format!("{}: {e}", path.display()))?;
            if entry.file_name() != c"." && entry.file_name() != c".." {
                return Err(format!(
                    "the data directory {} belongs to someone else and doesn't look like \
                     Lodestar's (no lodestar.db, config.toml or index), so its contents \
                     weren't handed to PUID/PGID. Point LODESTAR_DATA_DIR at an empty \
                     folder, or change the folder's owner yourself",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    /// Every entry is opened without following symlinks, then checked and
    /// changed through that descriptor, so nothing swapped in between
    /// the check and the change is touched, and nothing outside `dir` is
    /// reached through a link.
    pub(super) fn chown_tree(
        dir: rustix::fd::BorrowedFd<'_>,
        path: &Path,
        ids: Ids,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<(), String> {
        let (uid, gid) = (Uid::from_raw(ids.uid), Gid::from_raw(ids.gid));
        let fail = |what: &str, name: &CStr, e: rustix::io::Errno| {
            format!(
                "{what} {}: {e}",
                path.join(name.to_string_lossy().as_ref()).display()
            )
        };
        let mut entries = Dir::read_from(dir).map_err(|e| format!("{}: {e}", path.display()))?;
        while let Some(entry) = entries.read() {
            let entry = entry.map_err(|e| format!("{}: {e}", path.display()))?;
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            let fd: OwnedFd = rustix::fs::openat(
                dir,
                name,
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| fail("opening", name, e))?;
            let st = rustix::fs::fstat(&fd).map_err(|e| fail("stat", name, e))?;
            let kind = FileType::from_raw_mode(st.st_mode);
            match kind {
                FileType::Directory | FileType::Symlink => {}
                FileType::RegularFile if st.st_nlink <= 1 => {}
                FileType::RegularFile => {
                    walk.hard_links += 1;
                    continue;
                }
                _ => {
                    walk.special += 1;
                    continue;
                }
            }
            if (st.st_uid, st.st_gid) != (ids.uid, ids.gid) {
                // An O_PATH descriptor with AT_EMPTY_PATH: the inode just
                // checked, or the link itself for a symlink.
                rustix::fs::chownat(&fd, c"", Some(uid), Some(gid), AtFlags::EMPTY_PATH)
                    .map_err(|e| fail("chown", name, e))?;
                walk.changed += 1;
            }
            if kind == FileType::Directory {
                if depth + 1 >= MAX_DEPTH {
                    walk.too_deep += 1;
                    continue;
                }
                let sub = rustix::fs::openat(
                    &fd,
                    c".",
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|e| fail("opening", name, e))?;
                let child = path.join(name.to_string_lossy().as_ref());
                chown_tree(sub.as_fd(), &child, ids, depth + 1, walk)?;
            }
        }
        Ok(())
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod imp {
    use super::{Report, Scope};

    /// No per-thread set*id syscalls without `unsafe` here: root is refused
    /// rather than kept quietly.
    pub(super) fn run(
        _scope: Scope,
        env: &impl Fn(&str) -> Option<String>,
    ) -> Result<Report, String> {
        use rustix::process::{geteuid, getgid, getuid};
        let (puid, pgid) = (env("PUID"), env("PGID"));
        if !geteuid().is_root() {
            return Ok(Report::unprivileged(
                puid.as_deref(),
                pgid.as_deref(),
                getuid().as_raw(),
                getgid().as_raw(),
            ));
        }
        if env("LODESTAR_ALLOW_ROOT").as_deref() == Some("1") {
            let mut report = Report::default();
            report.warn.push(
                "running as root (LODESTAR_ALLOW_ROOT=1): privileges were not dropped".into(),
            );
            return Ok(report);
        }
        Err(
            "started as root, and this platform can't drop privileges; run it as an \
             unprivileged user (or set LODESTAR_ALLOW_ROOT=1)"
                .into(),
        )
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{Report, Scope};

    /// Windows has no root to drop; PUID/PGID mean nothing there.
    pub(super) fn run(
        _scope: Scope,
        _env: &impl Fn(&str) -> Option<String>,
    ) -> Result<Report, String> {
        Ok(Report::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_default_to_nonroot() {
        let ids = Ids {
            uid: DEFAULT_ID,
            gid: DEFAULT_ID,
        };
        assert_eq!(parse_ids(None, None, false), Ok(ids));
        assert_eq!(parse_ids(Some(""), Some(""), false), Ok(ids));
        assert_eq!(
            parse_ids(Some("99"), None, false),
            Ok(Ids {
                uid: 99,
                gid: DEFAULT_ID
            })
        );
        assert_eq!(
            parse_ids(Some("99"), Some("100"), false),
            Ok(Ids { uid: 99, gid: 100 })
        );
        assert_eq!(
            parse_ids(Some("4294967294"), Some("007"), false),
            Ok(Ids {
                uid: 4_294_967_294,
                gid: 7
            })
        );
    }

    #[test]
    fn ids_must_be_plain_numbers() {
        for bad in [
            "abc",
            "-1",
            "+99",
            " 99",
            "99 ",
            "0x63",
            "9 9",
            "99\n",
            "١٢",
            "4294967296",
            "99999999999999999999",
        ] {
            assert!(parse_ids(Some(bad), None, true).is_err(), "PUID={bad:?}");
            assert!(parse_ids(None, Some(bad), true).is_err(), "PGID={bad:?}");
        }
        let e = parse_ids(Some("4294967295"), None, true).unwrap_err();
        assert!(e.contains("reserved"), "{e}");
        let e = parse_ids(None, Some("x"), false).unwrap_err();
        assert!(e.starts_with("PGID="), "{e}");
    }

    #[test]
    fn root_needs_the_override() {
        for (u, g) in [
            (Some("0"), None),
            (None, Some("0")),
            (Some("00"), Some("5")),
        ] {
            let e = parse_ids(u, g, false).unwrap_err();
            assert!(e.contains("LODESTAR_ALLOW_ROOT=1"), "{e}");
        }
        assert_eq!(
            parse_ids(Some("0"), Some("0"), true),
            Ok(Ids { uid: 0, gid: 0 })
        );
    }

    #[test]
    fn unprivileged_logs_only_when_ids_differ() {
        let lines = |u, g| Report::unprivileged(u, g, 1000, 1000).info.len();
        assert_eq!(lines(None, None), 0);
        assert_eq!(lines(Some(""), Some("")), 0);
        assert_eq!(lines(Some("1000"), Some("1000")), 0);
        assert_eq!(lines(Some("99"), None), 1);
        assert_eq!(lines(None, Some("100")), 1);
        assert_eq!(lines(Some("99"), Some("100")), 1);
        assert_eq!(lines(Some("junk"), None), 1);
    }

    #[test]
    fn data_dir_follows_the_environment() {
        assert_eq!(env_data_dir(&|_| None), PathBuf::from("data"));
        assert_eq!(
            env_data_dir(&|k| (k == "LODESTAR_DATA_DIR").then(|| "/data".into())),
            PathBuf::from("/data")
        );
    }

    #[test]
    fn reads_status_fields() {
        let status = "Name:\tlodestar\nUid:\t99\t99\t99\t99\nCapEff:\t0000000000000000\n";
        assert_eq!(status_field(status, "Uid"), Some("99\t99\t99\t99"));
        assert_eq!(status_field(status, "CapEff"), Some("0000000000000000"));
        assert_eq!(status_field(status, "Cap"), None);
        assert_eq!(status_field(status, "Gid"), None);
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use std::os::unix::fs::{MetadataExt, symlink};

        use super::super::imp::{can_drop, check_no_caps, chown_data_dir};
        use super::super::*;

        #[test]
        fn capabilities_must_be_gone() {
            let ok = "CapInh:\t0000000000000000\nCapPrm:\t0000000000000000\n\
                      CapEff:\t0000000000000000\nCapAmb:\t0000000000000000\n";
            assert_eq!(check_no_caps(ok), Ok(()));
            let old_kernel = "CapPrm:\t0000000000000000\nCapEff:\t0000000000000000\n";
            assert_eq!(check_no_caps(old_kernel), Ok(()));
            let kept = "CapPrm:\t0000000000000000\nCapEff:\t0000000000000400\nCapAmb:\t0\n";
            assert!(check_no_caps(kept).unwrap_err().contains("CapEff"));
            let amb = "CapPrm:\t0\nCapEff:\t0\nCapAmb:\t0000000000000001\n";
            assert!(check_no_caps(amb).unwrap_err().contains("CapAmb"));
            assert!(check_no_caps("CapEff:\t0\n").is_err());
            assert!(check_no_caps("CapPrm:\t\nCapEff:\t0\n").is_err());
        }

        #[test]
        fn the_drop_needs_setuid_and_setgid() {
            assert_eq!(can_drop("CapEff:\t000001ffffffffff\n"), Ok(()));
            assert_eq!(can_drop("CapEff:\t00000000000000c0\n"), Ok(()));
            for missing in ["0000000000000000", "0000000000000040", "0000000000000080"] {
                let e = can_drop(&format!("CapEff:\t{missing}\n")).unwrap_err();
                assert!(e.contains("SETUID/SETGID") && e.contains("--user"), "{e}");
            }
            assert!(can_drop("CapPrm:\t0\n").is_err());
            assert!(can_drop("CapEff:\tzz\n").is_err());
        }

        #[test]
        fn a_symlinked_data_dir_is_refused() {
            let tmp = tempfile::tempdir().unwrap();
            let real = tmp.path().join("real");
            std::fs::create_dir(&real).unwrap();
            let link = tmp.path().join("link");
            symlink(&real, &link).unwrap();
            let ids = Ids { uid: 1, gid: 1 };
            let e = chown_data_dir(&link, ids).unwrap_err();
            assert!(e.contains("symlink"), "{e}");
            let file = tmp.path().join("file");
            std::fs::write(&file, b"").unwrap();
            assert!(chown_data_dir(&file, ids).is_err());
        }

        fn is_root() -> bool {
            if rustix::process::geteuid().is_root() {
                true
            } else {
                eprintln!("skipping: chown needs root (run the tests as root to cover it)");
                false
            }
        }

        fn owner(p: &std::path::Path) -> (u32, u32) {
            let m = std::fs::symlink_metadata(p).unwrap();
            (m.uid(), m.gid())
        }

        /// Root-only: chown is a privileged call. The tree is root's; the
        /// walk hands it to 4242:4343 without leaving it.
        #[test]
        fn the_tree_changes_owner_without_leaving_it() {
            if !is_root() {
                return;
            }
            let tmp = tempfile::tempdir().unwrap();
            let outside = tmp.path().join("outside");
            std::fs::write(&outside, b"secret").unwrap();
            let outside_dir = tmp.path().join("outside-dir");
            std::fs::create_dir(&outside_dir).unwrap();
            let data = tmp.path().join("data");
            std::fs::create_dir_all(data.join("index/seg")).unwrap();
            std::fs::write(data.join("lodestar.db"), b"").unwrap();
            std::fs::write(data.join("index/seg/a"), b"").unwrap();
            symlink(&outside, data.join("link")).unwrap();
            symlink(&outside_dir, data.join("dirlink")).unwrap();
            std::fs::hard_link(&outside, data.join("hard")).unwrap();

            let ids = Ids {
                uid: 4242,
                gid: 4343,
            };
            let line = chown_data_dir(&data, ids).unwrap();
            assert!(line.contains("was owned by 0:0"), "{line}");
            // data, index, index/seg, index/seg/a, lodestar.db, link, dirlink
            assert!(line.contains("changed 7 entries"), "{line}");
            assert!(line.contains("left 1 hard-linked files"), "{line}");
            for p in [
                "",
                "index",
                "index/seg",
                "index/seg/a",
                "lodestar.db",
                "link",
                "dirlink",
            ] {
                assert_eq!(owner(&data.join(p)), (4242, 4343), "{p}");
            }
            // Nothing reached through a link changed.
            assert_eq!(owner(&outside), (0, 0));
            assert_eq!(owner(&outside_dir), (0, 0));

            // Owned already: nothing is walked, even a stray root file.
            std::fs::write(data.join("stray"), b"").unwrap();
            let line = chown_data_dir(&data, ids).unwrap();
            assert!(line.contains("is owned by 4242:4343"), "{line}");
            assert_eq!(owner(&data.join("stray")), (0, 0));

            // A folder that isn't Lodestar's is left alone, contents and
            // all, until it has a marker.
            let other = tmp.path().join("srv");
            std::fs::create_dir_all(other.join("www")).unwrap();
            std::fs::write(other.join("www/site"), b"").unwrap();
            let e = chown_data_dir(&other, ids).unwrap_err();
            assert!(e.contains("doesn't look like Lodestar's"), "{e}");
            assert_eq!(owner(&other), (0, 0));
            assert_eq!(owner(&other.join("www/site")), (0, 0));
            std::fs::write(other.join("config.toml"), b"").unwrap();
            chown_data_dir(&other, ids).unwrap();
            assert_eq!(owner(&other.join("www/site")), (4242, 4343));
            // An empty one is fine.
            let empty = tmp.path().join("empty");
            std::fs::create_dir(&empty).unwrap();
            chown_data_dir(&empty, ids).unwrap();
            assert_eq!(owner(&empty), (4242, 4343));

            // Never the whole filesystem.
            assert!(
                chown_data_dir(Path::new("/"), ids)
                    .unwrap_err()
                    .contains("root")
            );
            assert!(
                chown_data_dir(Path::new("/proc/.."), ids)
                    .unwrap_err()
                    .contains("root")
            );

            // Missing: created and handed over.
            let fresh = tmp.path().join("new/data");
            chown_data_dir(&fresh, ids).unwrap();
            assert_eq!(owner(&fresh), (4242, 4343));
        }

        #[test]
        fn deep_trees_stop_at_the_limit() {
            if !is_root() {
                return;
            }
            let tmp = tempfile::tempdir().unwrap();
            let data = tmp.path().join("data");
            let mut deep = data.clone();
            for _ in 0..40 {
                deep.push("d");
            }
            std::fs::create_dir_all(&deep).unwrap();
            std::fs::write(data.join("lodestar.db"), b"").unwrap();
            let line = chown_data_dir(&data, Ids { uid: 7, gid: 7 }).unwrap();
            assert!(
                line.contains("left 1 directories nested too deep"),
                "{line}"
            );
            assert_eq!(owner(&deep), (0, 0));
        }
    }
}
