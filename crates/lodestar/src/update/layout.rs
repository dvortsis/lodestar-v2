//! `<data>/bin` (architecture §12.2): the supervisor's and the updater's
//! files, and the primitives every write goes through.
//!
//! The folder is created 0700 and refused if it's a symlink or owned by
//! another user. Every file is written by temporary file, fsync, rename
//! and a directory fsync, and opened with `O_NOFOLLOW`. Overlay folder
//! names are validated versions, never paths.
//!
//! **Two writers, never at once:** the supervisor writes only while no
//! child runs (`active`, `pending`, `rollback.json`, `halted.json`,
//! `skipped` for failed versions); the child writes `probation.json`,
//! `request.json`, `migration.json` and deletes `pending` once its
//! self-test passes (and, with the updater, the rest).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestar_update::semver::Version;
use rustix::fs::{FlockOperation, OFlags};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::log_events::UPDATE_DIR_SYNC_UNSUPPORTED;

/// The largest state file read (`history.json` is the biggest).
pub const MAX_STATE: u64 = 256 * 1024;

/// `<data>/bin`, checked.
#[derive(Debug, Clone)]
pub struct Bin {
    dir: PathBuf,
    /// The folder can't be fsynced (and that was logged): once per `Bin`,
    /// which the engine opens once per run.
    dir_sync_unsupported: Arc<AtomicBool>,
}

/// The exclusive lock on `<data>/bin/.lock`, held while the value lives.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

impl Bin {
    /// `<data>/bin`, created 0700 if missing. Refused when it's a symlink,
    /// not a folder, or owned by another user.
    pub fn open(data_dir: &Path) -> io::Result<Bin> {
        let dir = data_dir.join("bin");
        match fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(data_dir)?;
                match fs::DirBuilder::new().mode(0o700).create(&dir) {
                    Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                    _ => {}
                }
            }
            Err(e) => return Err(e),
            Ok(_) => {}
        }
        Bin::check(dir)
    }

    /// An existing `<data>/bin`, checked, without creating it.
    pub fn existing(data_dir: &Path) -> io::Result<Option<Bin>> {
        let dir = data_dir.join("bin");
        match fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
            Ok(_) => Bin::check(dir).map(Some),
        }
    }

    /// Checks the folder through a descriptor opened without following a
    /// link, so the owner checked and the mode set are the folder's own.
    fn check(dir: PathBuf) -> io::Result<Bin> {
        let refuse = |why: String| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{}: {why}", dir.display()),
            )
        };
        let fd = rustix::fs::open(
            &dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| match e {
            rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => {
                refuse("not a folder (a symlink isn't accepted)".into())
            }
            e => io::Error::from(e),
        })?;
        let st = rustix::fs::fstat(&fd)?;
        let me = rustix::process::geteuid().as_raw();
        if st.st_uid != me {
            return Err(refuse(format!(
                "owned by user {}, not this process's user {me}. On a share or a drive \
                 without Unix owners (SMB, NTFS, exFAT), mount it with uid={me}, or set \
                 PUID to its owner",
                st.st_uid
            )));
        }
        if st.st_mode & 0o077 != 0 {
            rustix::fs::fchmod(&fd, rustix::fs::Mode::from_raw_mode(0o700)).map_err(|e| {
                refuse(format!(
                    "can't make it private (0700): {e}. A filesystem without Unix \
                     permissions can't hold it"
                ))
            })?;
        }
        Ok(Bin {
            dir,
            dir_sync_unsupported: Arc::default(),
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// An overlay's folder: the version's own canonical text, never a path.
    pub fn overlay(&self, version: &Version) -> PathBuf {
        self.dir.join(version.to_string())
    }

    /// Takes `.lock` exclusively, or `None` while another process holds it.
    pub fn lock(&self) -> io::Result<Option<Lock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(nofollow())
            .open(self.path(".lock"))?;
        match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Lock { _file: file })),
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Reads and parses a state file, `None` if it doesn't exist.
    pub fn read<T: DeserializeOwned>(&self, name: &str) -> io::Result<Option<T>> {
        read_json(&self.path(name))
    }

    /// Writes a state file durably (see [`write_atomic`]).
    /// Refused past [`MAX_STATE`], which a read would take for damage.
    pub fn write<T: Serialize>(&self, name: &str, value: &T) -> io::Result<()> {
        write_atomic(&self.dir, name, &state_bytes(name, value)?)
    }

    /// [`Bin::write`] for a write that's acknowledged to the user: a
    /// failed folder fsync is an error too, and `fault` can fail any step
    /// (tests; `|_| Ok(())` otherwise).
    pub fn write_acknowledged<T: Serialize>(
        &self,
        name: &str,
        value: &T,
        fault: &dyn Fn(Step) -> io::Result<()>,
    ) -> io::Result<()> {
        write_steps(&self.dir, name, &state_bytes(name, value)?, fault)?;
        self.sync_acknowledged(fault)
    }

    /// [`Bin::remove`] for a removal that's acknowledged to the user: a
    /// failed folder fsync is an error too.
    pub fn remove_acknowledged(
        &self,
        name: &str,
        fault: &dyn Fn(Step) -> io::Result<()>,
    ) -> io::Result<()> {
        fault(Step::Rename)?;
        match fs::remove_file(self.path(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        self.sync_acknowledged(fault)
    }

    /// The folder fsync after an acknowledged change. A filesystem that
    /// can't fsync a folder at all (`EINVAL`, `ENOTSUP`: some FUSE mounts,
    /// such as Unraid's `/mnt/user`) leaves the rename standing, as every
    /// other writer here does; that's logged once, without a path. Any
    /// other error (`EIO`, …) fails the change.
    fn sync_acknowledged(&self, fault: &dyn Fn(Step) -> io::Result<()>) -> io::Result<()> {
        let dir = File::open(&self.dir)?;
        match fault(Step::SyncDir).and_then(|()| dir.sync_all()) {
            Err(e) if sync_unsupported(&e) => {
                if !self.dir_sync_unsupported.swap(true, Ordering::Relaxed) {
                    lodestar_log::log_event!(UPDATE_DIR_SYNC_UNSUPPORTED);
                }
                Ok(())
            }
            r => r,
        }
    }

    /// Whether a folder fsync was found unsupported (and logged).
    #[cfg(test)]
    pub fn dir_sync_unsupported(&self) -> bool {
        self.dir_sync_unsupported.load(Ordering::Relaxed)
    }

    /// Removes a state file (and makes that durable); fine if it's gone.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.path(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => {
                sync_dir(&self.dir);
                Ok(())
            }
        }
    }

    pub fn exists(&self, name: &str) -> bool {
        fs::symlink_metadata(self.path(name)).is_ok()
    }

    /// The overlay folders: names that are strict versions, each a real
    /// folder (`<version>.bad` and anything else is ignored). An entry that
    /// can't be read fails the whole list.
    pub fn overlays(&self) -> io::Result<Vec<Version>> {
        self.scan(Err)
    }

    /// [`Bin::overlays`], skipping (and handing to `bad`) an entry that
    /// can't be read, so one bad entry doesn't hide the others.
    pub fn overlays_skipping(&self, mut bad: impl FnMut(&io::Error)) -> io::Result<Vec<Version>> {
        self.scan(|e| {
            bad(&e);
            Ok(())
        })
    }

    fn scan(&self, mut bad: impl FnMut(io::Error) -> io::Result<()>) -> io::Result<Vec<Version>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    bad(e)?;
                    continue;
                }
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(v) = parse_version(&name) else {
                continue;
            };
            match entry.file_type() {
                Ok(t) if t.is_dir() => out.push(v),
                Ok(_) => {}
                Err(e) => bad(e)?,
            }
        }
        out.sort();
        Ok(out)
    }

    /// Deletes an overlay folder (only ever one named by a version).
    pub fn remove_overlay(&self, version: &Version) -> io::Result<()> {
        match fs::remove_dir_all(self.overlay(version)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => {
                sync_dir(&self.dir);
                Ok(())
            }
        }
    }
}

/// A version as an overlay folder or a state file names it: strict
/// semver without build metadata, at most 64 bytes, in its own canonical
/// form (so a name is never a path or an alias).
pub fn parse_version(s: &str) -> Option<Version> {
    if s.len() > 64 {
        return None;
    }
    let v = Version::parse(s).ok()?;
    (v.build.is_empty() && v.to_string() == s).then_some(v)
}

pub fn nofollow() -> i32 {
    OFlags::NOFOLLOW.bits() as i32
}

/// Opens a regular file for reading without following a symlink. It's
/// opened non-blocking, so a FIFO in its place can't hang the open, and
/// anything but a regular file is refused.
pub fn open_nofollow(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nofollow() | OFlags::NONBLOCK.bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} isn't a regular file", path.display()),
        ));
    }
    Ok(file)
}

/// Reads at most [`MAX_STATE`] bytes of JSON; `None` if the file is gone.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let file = match open_nofollow(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is too large", path.display()),
        ));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", path.display()),
        )
    })
}

/// Writes `dir/name` durably: a new temporary file (`O_NOFOLLOW`, 0600),
/// fsync, rename over the name, fsync the folder. A crash leaves the old
/// file or the new one, never a mix.
pub fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    write_steps(dir, name, bytes, &|_| Ok(()))?;
    sync_dir(dir);
    Ok(())
}

/// One step of [`Bin::write_acknowledged`], for a test to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Midway through writing the temporary file.
    Write,
    /// Before its fsync.
    Flush,
    /// Before it's renamed over the name (or, removing, before the
    /// file is removed).
    Rename,
    /// After the rename, before the folder's fsync.
    SyncDir,
}

/// A folder fsync this filesystem doesn't support, rather than a failure.
fn sync_unsupported(e: &io::Error) -> bool {
    use rustix::io::Errno;
    Errno::from_io_error(e)
        .is_some_and(|n| n == Errno::INVAL || n == Errno::NOTSUP || n == Errno::OPNOTSUPP)
}

fn state_bytes<T: Serialize>(name: &str, value: &T) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_STATE {
        return Err(io::Error::other(format!("{name} would be too large")));
    }
    Ok(bytes)
}

/// Writes `dir/name` up to the rename: a new temporary file (`O_NOFOLLOW`,
/// 0600), fsync, rename over the name, with `fault` asked before each
/// step. A failure leaves the old file. The caller fsyncs the folder.
fn write_steps(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    fault: &dyn Fn(Step) -> io::Result<()>,
) -> io::Result<()> {
    let tmp = dir.join(format!(".{name}.tmp"));
    let _ = fs::remove_file(&tmp);
    let written = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nofollow())
            .open(&tmp)?;
        let (head, tail) = bytes.split_at(bytes.len() / 2);
        f.write_all(head)?;
        fault(Step::Write)?;
        f.write_all(tail)?;
        fault(Step::Flush)?;
        f.sync_all()?;
        fault(Step::Rename)?;
        fs::rename(&tmp, dir.join(name))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

pub fn sync_dir(dir: &Path) {
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn creates_a_private_folder_and_refuses_a_symlink() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let mode = fs::metadata(bin.dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let other = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), other.path().join("bin")).unwrap();
        assert!(Bin::open(other.path()).is_err());
        assert!(Bin::existing(other.path()).is_err());
    }

    #[test]
    fn writes_are_atomic_and_never_follow_a_link() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        bin.write("active", &serde_json::json!({"version": "0.5.3"}))
            .unwrap();
        let back: serde_json::Value = bin.read("active").unwrap().unwrap();
        assert_eq!(back["version"], "0.5.3");
        assert!(bin.read::<serde_json::Value>("pending").unwrap().is_none());
        // A symlink in place of a state file isn't read through.
        let target = data.path().join("elsewhere");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, bin.path("pending")).unwrap();
        assert!(bin.read::<serde_json::Value>("pending").is_err());
        let names: Vec<_> = fs::read_dir(bin.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(names.iter().all(|n| !n.to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn a_fifo_or_an_oversize_file_is_damage_not_a_hang() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        // rustix has no mknodat/mkfifoat on macOS, and the workspace forbids
        // unsafe, so the FIFO comes from mkfifo(1), which every Unix ships.
        let made = std::process::Command::new("mkfifo")
            .arg(bin.path("pending"))
            .status()
            .unwrap();
        assert!(made.success(), "mkfifo failed: {made}");
        assert!(bin.read::<serde_json::Value>("pending").is_err());
        fs::write(bin.path("active"), vec![b' '; MAX_STATE as usize + 1]).unwrap();
        let e = bin.read::<serde_json::Value>("active").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let big = "x".repeat(MAX_STATE as usize);
        assert!(bin.write("skipped", &big).is_err());
        assert!(!bin.exists("skipped"));
    }

    #[test]
    fn the_lock_is_exclusive() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let held = bin.lock().unwrap().unwrap();
        assert!(bin.lock().unwrap().is_none());
        drop(held);
        assert!(bin.lock().unwrap().is_some());
    }

    #[test]
    fn overlay_names_are_canonical_versions_only() {
        assert!(parse_version("0.5.3").is_some());
        assert!(parse_version("0.5.3-nightly.20261023").is_some());
        for bad in [
            "../x",
            "0.5",
            "v0.5.3",
            "0.5.3+b",
            "00.5.3",
            "0.5.3.bad",
            "",
        ] {
            assert!(parse_version(bad).is_none(), "{bad}");
        }
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        for name in ["0.5.3", "0.6.0", "0.5.9.bad", "junk"] {
            fs::create_dir(bin.path(name)).unwrap();
        }
        fs::write(bin.path("0.7.0"), "a file").unwrap();
        let found: Vec<String> = bin
            .overlays()
            .unwrap()
            .iter()
            .map(|v| v.to_string())
            .collect();
        assert_eq!(found, ["0.5.3", "0.6.0"]);
    }
}
