//! The data folder's own mode (S8-01).
//!
//! The folder Lodestar creates for its data is owner-only on Unix (0700), so
//! every file under it is closed to other local users whatever mode each file
//! gets. No process-wide umask is set: outputs and companion files are meant
//! for other users to read. A folder that already exists is never changed;
//! one open to its group or to others gets one warning at start that names
//! the safe mode.

use std::io;
use std::path::Path;

/// The mode a new data folder gets on Unix.
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

/// Logs one warning when the data folder is open to its group or others.
/// It never changes the folder.
pub fn warn_if_open(dir: &Path) {
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
    fn a_new_folder_is_owner_only_whatever_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("data");
        ensure(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
        assert_eq!(open_mode(&dir), None);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_open_folder_is_reported_and_never_changed() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("data");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!ensure(&dir).unwrap());
        warn_if_open(&dir);
        assert_eq!(open_mode(&dir), Some(0o755));
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn a_file_in_the_way_is_an_error() {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("data");
        std::fs::write(&path, b"x").unwrap();
        assert!(ensure(&path).is_err());
    }
}
