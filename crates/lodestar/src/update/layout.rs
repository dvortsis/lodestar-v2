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

use lodestar_update::semver::Version;
use rustix::fs::{FlockOperation, OFlags};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The largest state file read (`history.json` is the biggest).
pub const MAX_STATE: u64 = 256 * 1024;

/// `<data>/bin`, checked.
#[derive(Debug, Clone)]
pub struct Bin {
    dir: PathBuf,
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
        Ok(Bin { dir })
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
        let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_STATE {
            return Err(io::Error::other(format!("{name} would be too large")));
        }
        write_atomic(&self.dir, name, &bytes)
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
    /// folder (`<version>.bad` and anything else is ignored).
    pub fn overlays(&self) -> io::Result<Vec<Version>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(v) = parse_version(&name) else {
                continue;
            };
            if entry.file_type()?.is_dir() {
                out.push(v);
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
    let tmp = dir.join(format!(".{name}.tmp"));
    let _ = fs::remove_file(&tmp);
    let written = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nofollow())
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, dir.join(name))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written?;
    sync_dir(dir);
    Ok(())
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
        rustix::fs::mknodat(
            rustix::fs::CWD,
            bin.path("pending"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
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
