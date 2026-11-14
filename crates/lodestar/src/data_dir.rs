//! The data folder's own mode (S8-01).
//!
//! The data folder is owner-only on Unix (0700), so every file under it is
//! closed to other local users whatever mode each file gets. A folder
//! Lodestar creates is made so. An existing one Lodestar's user owns is
//! tightened at start, as the logs folder is (`logging/file.rs`). One it
//! doesn't own is never changed: if it's open to its group or to others, the
//! start logs one warning that names the safe mode. No process-wide umask is
//! set: outputs and companion files are meant for other users to read.

use std::io;
use std::path::Path;

/// The mode the data folder gets on Unix.
#[cfg(unix)]
pub const MODE: u32 = 0o700;

/// Creates `dir` if it's missing: its missing parents with the default
/// mode, the folder itself owner-only on Unix. An existing folder is left as
/// it is. Returns whether it created the folder.
pub fn ensure(dir: &Path) -> io::Result<bool> {
    if dir.is_dir() {
        return Ok(false);
    }
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    let builder = {
        let mut builder = std::fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, MODE);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    match builder.create(dir) {
        Ok(()) => Ok(true),
        // Created meanwhile by another process: left as it is, like any existing folder.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(false),
        Err(e) => Err(e),
    }
}

/// What the start does with an existing data folder.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Owner-only already: nothing to do.
    Keep,
    /// Open, and owned by Lodestar's user: made owner-only.
    Tighten,
    /// Open, and owned by another account: left as it is, with a warning.
    Warn,
}

/// The decision for a folder with permission bits `mode`, owned by `owner`,
/// when Lodestar runs as `euid`.
#[cfg(unix)]
pub fn action(mode: u32, owner: u32, euid: u32) -> Action {
    match (mode & 0o077 == 0, owner == euid) {
        (true, _) => Action::Keep,
        (false, true) => Action::Tighten,
        (false, false) => Action::Warn,
    }
}

/// Makes the existing data folder owner-only when Lodestar's user owns it.
/// It's changed through a descriptor opened without following a link, so a
/// symlink is never followed for the change. Returns what it did; anything
/// it can't vouch for (a symlink, a folder it can't open) is `Warn`, and
/// the caller warns only if the folder really is open.
#[cfg(unix)]
pub fn tighten(dir: &Path) -> Action {
    use rustix::fs::{Mode, OFlags};
    let Ok(fd) = rustix::fs::open(
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) else {
        return Action::Warn;
    };
    let Ok(st) = rustix::fs::fstat(&fd) else {
        return Action::Warn;
    };
    #[allow(
        clippy::unnecessary_cast,
        reason = "st_mode is u16 on some Unix targets"
    )]
    let mode = st.st_mode as u32 & 0o777;
    match action(mode, st.st_uid, rustix::process::geteuid().as_raw()) {
        Action::Tighten => match rustix::fs::fchmod(&fd, Mode::from_raw_mode(MODE as _)) {
            Ok(()) => Action::Tighten,
            Err(_) => Action::Warn,
        },
        other => other,
    }
}

/// The folder's permission bits when its group or others have any access,
/// else `None` (and always `None` off Unix).
pub fn open_mode(dir: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir).ok()?.permissions().mode() & 0o777;
        (mode & 0o077 != 0).then_some(mode)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// At start: tightens the data folder when Lodestar's user owns it, and logs
/// one warning when it's still open to its group or others.
pub fn secure(dir: &Path) {
    #[cfg(unix)]
    if tighten(dir) != Action::Warn {
        return;
    }
    if open_mode(dir).is_some() {
        use crate::logging::events;
        use lodestar_log::{Path as LogPath, log_event};
        log_event!(events::SYSTEM_DATA_DIR_OPEN, path = LogPath::new(dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_folder_is_created_and_an_existing_one_is_left() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("a").join("data");
        assert!(ensure(&dir).unwrap());
        assert!(dir.is_dir());
        assert!(!ensure(&dir).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn only_an_open_folder_lodestars_user_owns_is_tightened() {
        assert_eq!(action(0o700, 7, 7), Action::Keep);
        assert_eq!(action(0o700, 8, 7), Action::Keep);
        assert_eq!(action(0o755, 7, 7), Action::Tighten);
        assert_eq!(action(0o750, 7, 7), Action::Tighten);
        assert_eq!(action(0o755, 8, 7), Action::Warn);
        assert_eq!(action(0o701, 8, 7), Action::Warn);
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_new_folder_is_owner_only_whatever_the_umask() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("data");
        ensure(&dir).unwrap();
        assert_eq!(mode_of(&dir) & 0o077, 0, "mode {:o}", mode_of(&dir));
        assert_eq!(open_mode(&dir), None);
    }

    #[cfg(unix)]
    #[test]
    fn an_open_folder_lodestars_user_owns_becomes_owner_only() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("data");
        std::fs::create_dir(&dir).unwrap();
        set_mode(&dir, 0o755);
        assert_eq!(tighten(&dir), Action::Tighten);
        assert_eq!(mode_of(&dir), 0o700);
        assert_eq!(tighten(&dir), Action::Keep);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_folder_is_never_changed_and_is_warned_about() {
        crate::test_log::install();
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        std::fs::create_dir(&real).unwrap();
        set_mode(&real, 0o755);
        let link = base.path().join("data-via-a-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(!ensure(&link).unwrap());
        assert_eq!(tighten(&link), Action::Warn);
        secure(&link);
        assert_eq!(mode_of(&real), 0o755);
        let log = crate::test_log::text();
        assert!(log.contains("system.data_dir_open"), "{log}");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_an_error() {
        let base = tempfile::tempdir().unwrap();
        let link = base.path().join("data");
        std::os::unix::fs::symlink(base.path().join("missing"), &link).unwrap();
        assert!(ensure(&link).is_err());
    }

    #[test]
    fn a_file_in_the_way_is_an_error() {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("data");
        std::fs::write(&path, b"x").unwrap();
        assert!(ensure(&path).is_err());
    }
}
