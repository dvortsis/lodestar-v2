//! `<data>/bin` (architecture §12.2): the supervisor's and the updater's
//! files, and the primitives every write goes through.
//!
//! **Unix:** the folder is created 0700 and refused if it's a symlink or
//! owned by another user. Every file is written by temporary file, fsync,
//! rename and a directory fsync, and opened with `O_NOFOLLOW`. Overlay
//! folder names are validated versions, never paths.
//!
//! **Windows** (`<data>\bin`, PK-win-upd-1), the same guarantees with
//! std's safe Windows calls (the workspace forbids `unsafe`):
//! - **No reparse points:** the folder and every file in it are opened
//!   with `FILE_FLAG_OPEN_REPARSE_POINT`, so a symlink or junction is
//!   opened as itself, and refused by its attributes on the handle (no
//!   check-then-open gap). `<data>` itself is the configured folder, as on
//!   Unix. Only the service's default folder is checked for reparse
//!   points at start (`windows::acl`); a folder set with `--config` or
//!   `LODESTAR_DATA_DIR` isn't.
//! - **Pinned:** [`Bin`] holds the folder open without
//!   `FILE_SHARE_DELETE` while it lives, so it can't be renamed, deleted
//!   or swapped for a junction after it was checked.
//! - **Atomic replace:** temporary file (`CREATE_NEW`, never through a
//!   link), `FlushFileBuffers`, then `MoveFileEx`-style replace
//!   (`std::fs::rename`). There's no folder flush (std can't open a
//!   folder for one), and NTFS writes its log lazily, so a power cut
//!   right after can bring back the previous file: atomic, but an
//!   acknowledged rename isn't guaranteed to survive power loss.
//! - **One writer:** `.lock`, an exclusive `LockFileEx` lock
//!   (`File::try_lock`), held open without `FILE_SHARE_DELETE` so it
//!   can't be renamed away and replaced by an unlocked one.
//! - **Owner-only:** rests on the folder's ACL, not on code: files inherit
//!   the data folder's protected DACL, which the MSI sets and the service
//!   checks at every start (SYSTEM, Administrators and `NT SERVICE\Lodestar`
//!   only; `secure-datadir.ps1`, `windows::acl`). The portable build's
//!   folder has whatever ACL the place it was unpacked to gives it. Setting
//!   an ACL from code would need Win32 calls (`unsafe`) or a new crate.
//!
//! **Two writers, never at once:** the supervisor writes only while no
//! child runs (`active`, `pending`, `rollback.json`, `halted.json`,
//! `skipped` for failed versions); the child writes `probation.json`,
//! `request.json`, `migration.json` and deletes `pending` once its
//! self-test passes (and, with the updater, the rest).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

use lodestar_update::semver::Version;
#[cfg(unix)]
use rustix::fs::{FlockOperation, OFlags};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[cfg(unix)]
use super::log_events::UPDATE_DIR_SYNC_UNSUPPORTED;

/// The largest state file read (`history.json` is the biggest).
pub const MAX_STATE: u64 = 256 * 1024;

/// `<data>/bin`, checked.
#[derive(Debug, Clone)]
pub struct Bin {
    dir: PathBuf,
    /// The folder can't be fsynced (and that was logged): once per `Bin`,
    /// which the engine opens once per run.
    #[cfg(unix)]
    dir_sync_unsupported: Arc<AtomicBool>,
    /// The folder, held open without `FILE_SHARE_DELETE` so it can't be
    /// renamed or replaced while this lives.
    #[cfg(windows)]
    _pin: Arc<File>,
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
                crate::data_dir::ensure(data_dir)?;
                #[cfg(unix)]
                let builder = {
                    let mut b = fs::DirBuilder::new();
                    b.mode(0o700);
                    b
                };
                #[cfg(not(unix))]
                let builder = fs::DirBuilder::new();
                match builder.create(&dir) {
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
    #[cfg(unix)]
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

    /// Opens the folder as itself (never through a link or junction),
    /// refuses it unless it's a real folder, and keeps it open so it
    /// can't be renamed or replaced afterwards.
    #[cfg(windows)]
    fn check(dir: PathBuf) -> io::Result<Bin> {
        use std::os::windows::fs::OpenOptionsExt;
        let pin = OpenOptions::new()
            .read(true)
            .share_mode(win::SHARE_READ | win::SHARE_WRITE)
            .custom_flags(win::BACKUP_SEMANTICS | win::OPEN_REPARSE_POINT)
            .open(&dir)?;
        let attrs = win::attributes(&pin)?;
        if attrs & win::ATTRIBUTE_REPARSE_POINT != 0 || attrs & win::ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{}: not a folder (a link or junction isn't accepted)",
                    dir.display()
                ),
            ));
        }
        Ok(Bin {
            dir,
            _pin: Arc::new(pin),
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
    #[cfg(unix)]
    pub fn overlay(&self, version: &Version) -> PathBuf {
        self.dir.join(version.to_string())
    }

    /// Takes `.lock` exclusively, or `None` while another process holds it.
    #[cfg(unix)]
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

    /// Takes `.lock` exclusively (`LockFileEx`), or `None` while another
    /// handle holds it. A link or junction in its place is refused.
    #[cfg(windows)]
    pub fn lock(&self) -> io::Result<Option<Lock>> {
        use std::os::windows::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(win::SHARE_READ | win::SHARE_WRITE)
            .custom_flags(win::OPEN_REPARSE_POINT)
            .open(self.path(".lock"))?;
        win::refuse_reparse(&file, &self.path(".lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Lock { _file: file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(e),
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
    #[cfg(unix)]
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

    /// std can't flush a folder (see the module's note on power loss):
    /// only the test fault runs.
    #[cfg(windows)]
    fn sync_acknowledged(&self, fault: &dyn Fn(Step) -> io::Result<()>) -> io::Result<()> {
        fault(Step::SyncDir)
    }

    /// Whether a folder fsync was found unsupported (and logged).
    #[cfg(all(test, unix))]
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
    #[cfg(unix)]
    pub fn overlays(&self) -> io::Result<Vec<Version>> {
        self.scan(Err)
    }

    /// [`Bin::overlays`], skipping (and handing to `bad`) an entry that
    /// can't be read, so one bad entry doesn't hide the others.
    #[cfg(unix)]
    pub fn overlays_skipping(&self, mut bad: impl FnMut(&io::Error)) -> io::Result<Vec<Version>> {
        self.scan(|e| {
            bad(&e);
            Ok(())
        })
    }

    #[cfg(unix)]
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
    #[cfg(unix)]
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

#[cfg(unix)]
pub fn nofollow() -> i32 {
    OFlags::NOFOLLOW.bits() as i32
}

/// Opens a regular file for reading without following a symlink. It's
/// opened non-blocking, so a FIFO in its place can't hang the open, and
/// anything but a regular file is refused.
#[cfg(unix)]
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

/// Opens a regular file for reading as itself: a symlink, junction or
/// other reparse point is opened, not followed, and refused, as is
/// anything but a regular file.
#[cfg(windows)]
pub fn open_nofollow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(win::OPEN_REPARSE_POINT)
        .open(path)?;
    win::refuse_reparse(&file, path)?;
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
#[cfg(unix)]
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
        let mut f = create_private(&tmp)?;
        let (head, tail) = bytes.split_at(bytes.len() / 2);
        f.write_all(head)?;
        fault(Step::Write)?;
        f.write_all(tail)?;
        fault(Step::Flush)?;
        f.sync_all()?;
        fault(Step::Rename)?;
        rename_retrying(&tmp, &dir.join(name))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Opens the folder `path` as itself and holds it: never through a link
/// or junction (the handle's own attributes are checked), and on Windows
/// without `FILE_SHARE_DELETE`, so while the handle lives the folder
/// can't be renamed, deleted or replaced by a junction. The update helper
/// pins `<data>` and `<data>\updates` this way before it reads or writes
/// beneath them as SYSTEM. On Unix it only refuses a link.
pub fn pin_dir(path: &Path) -> io::Result<File> {
    let refused = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: not a folder (a link or junction isn't accepted)",
                path.display()
            ),
        )
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let pin = OpenOptions::new()
            .read(true)
            .share_mode(win::SHARE_READ | win::SHARE_WRITE)
            .custom_flags(win::BACKUP_SEMANTICS | win::OPEN_REPARSE_POINT)
            .open(path)?;
        let attrs = win::attributes(&pin)?;
        if attrs & win::ATTRIBUTE_REPARSE_POINT != 0 || attrs & win::ATTRIBUTE_DIRECTORY == 0 {
            return Err(refused());
        }
        Ok(pin)
    }
    #[cfg(unix)]
    {
        let fd = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| match e {
            rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => refused(),
            e => io::Error::from(e),
        })?;
        Ok(File::from(fd))
    }
}

/// Renames `from` to `to`. On Windows a rename refused with a sharing
/// conflict or a denied access (os errors 32 and 5) is retried for about
/// 1.3 s: antivirus and the search indexer hold a new file open for a
/// moment (architecture B5a), as `lodestar-search`'s index renames allow
/// for. Any other error comes back at once, and the last one when the
/// retries run out. Elsewhere it's `fs::rename`.
pub fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        retry(win::sharing_conflict, || fs::rename(from, to))
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)
    }
}

/// The attempts [`rename_retrying`] makes, and the first pause between
/// two; each pause doubles, up to [`RETRY_MAX_PAUSE`].
const RETRY_ATTEMPTS: u32 = 8;
const RETRY_FIRST_PAUSE: std::time::Duration = std::time::Duration::from_millis(5);
const RETRY_MAX_PAUSE: std::time::Duration = std::time::Duration::from_millis(320);

/// Runs `op` until it succeeds, fails with an error `transient` doesn't
/// accept, or has made [`RETRY_ATTEMPTS`] attempts (the last error is
/// returned).
#[cfg_attr(not(windows), allow(dead_code))]
fn retry(
    transient: fn(&io::Error) -> bool,
    mut op: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut pause = RETRY_FIRST_PAUSE;
    let mut attempt = 1;
    loop {
        match op() {
            Err(e) if attempt < RETRY_ATTEMPTS && transient(&e) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(RETRY_MAX_PAUSE);
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// A new file (never an existing one, and never through a link): 0600 on
/// Unix; on Windows it inherits the folder's ACL.
#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nofollow())
        .open(path)
}

#[cfg(windows)]
fn create_private(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(win::OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(unix)]
pub fn sync_dir(dir: &Path) {
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
}

/// std can't open a folder to flush it (see the module's note on power
/// loss).
#[cfg(windows)]
pub fn sync_dir(_dir: &Path) {}

/// The Win32 constants std's `OpenOptionsExt` takes, and the attribute
/// check every open makes.
#[cfg(windows)]
mod win {
    use std::fs::File;
    use std::io;
    use std::os::windows::fs::MetadataExt;
    use std::path::Path;

    pub const SHARE_READ: u32 = 0x0000_0001;
    pub const SHARE_WRITE: u32 = 0x0000_0002;
    pub const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    pub const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    pub const ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    pub const ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

    /// The attributes of the object the handle names.
    pub fn attributes(file: &File) -> io::Result<u32> {
        Ok(file.metadata()?.file_attributes())
    }

    /// A rename over a file another handle holds: `ERROR_ACCESS_DENIED`
    /// (5) or `ERROR_SHARING_VIOLATION` (32).
    pub fn sharing_conflict(e: &io::Error) -> bool {
        matches!(e.raw_os_error(), Some(5 | 32))
    }

    /// Refuses a handle to a link, junction or other reparse point.
    pub fn refuse_reparse(file: &File, path: &Path) -> io::Result<()> {
        if attributes(file)? & ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is a link or junction", path.display()),
            ));
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
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
    fn a_missing_data_folder_is_created_owner_only() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("data");
        Bin::open(&data).unwrap();
        let mode = fs::metadata(&data).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {:o}", mode & 0o777);
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

/// The Windows layout (run on the Windows CI job): reparse points refused,
/// atomic replace, one writer, and the folder pinned.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// A held file refuses the rename with 32; once it's let go (here,
    /// right after the first refusal) the retry goes through. No waiting
    /// on a clock: the release follows the refusal itself.
    #[test]
    fn a_briefly_held_file_is_renamed_over_once_its_let_go() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (tmp, dst) = (dir.path().join("new"), dir.path().join("state"));
        fs::write(&tmp, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        // The new file held open with read sharing only (no
        // FILE_SHARE_DELETE), as a scanner holds it: renaming it is a
        // sharing violation, 32. (A held destination answers 5, which is
        // retried too.)
        let held = OpenOptions::new()
            .read(true)
            .share_mode(win::SHARE_READ)
            .open(&tmp)
            .unwrap();
        let held = std::cell::RefCell::new(Some(held));
        let mut refusals = Vec::new();
        retry(win::sharing_conflict, || {
            let r = fs::rename(&tmp, &dst);
            if let Err(e) = &r {
                refusals.push(e.raw_os_error());
                held.borrow_mut().take();
            }
            r
        })
        .unwrap();
        assert_eq!(refusals, [Some(32)]);
        assert_eq!(fs::read(&dst).unwrap(), b"new");
        // A missing source isn't retried.
        let mut n = 0;
        let e = retry(win::sharing_conflict, || {
            n += 1;
            fs::rename(dir.path().join("gone"), &dst)
        })
        .unwrap_err();
        assert_eq!((n, e.kind()), (1, io::ErrorKind::NotFound));
        // rename_retrying itself, end to end.
        fs::write(&tmp, b"newer").unwrap();
        rename_retrying(&tmp, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"newer");
    }

    /// A directory junction at `link` to `target` (`mklink /J` needs no
    /// privilege, unlike a symlink).
    fn junction(link: &Path, target: &Path) {
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn a_junction_in_place_of_the_folder_or_a_file_is_refused() {
        let data = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        junction(&data.path().join("bin"), elsewhere.path());
        assert!(Bin::open(data.path()).is_err());
        assert!(Bin::existing(data.path()).is_err());

        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("x"), "{}").unwrap();
        // A reparse point where a state file belongs is opened as itself
        // and refused, never read through.
        junction(&bin.path("trust.json"), target.path());
        assert!(open_nofollow(&bin.path("trust.json")).is_err());
        assert!(bin.read::<serde_json::Value>("trust.json").is_err());
        junction(&bin.path(".lock"), target.path());
        assert!(bin.lock().is_err());
    }

    /// A file symlink (GitHub's Windows runners are administrators, who
    /// may make one).
    fn symlink_file(link: &Path, target: &Path) {
        std::os::windows::fs::symlink_file(target, link)
            .unwrap_or_else(|e| panic!("symlink_file {}: {e}", link.display()));
    }

    /// File links in place of state files are opened as themselves and
    /// refused: never read through, never locked through, never written
    /// through.
    #[test]
    fn a_file_symlink_is_never_followed() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("elsewhere.json");
        fs::write(&secret, br#"{"format":1}"#).unwrap();
        symlink_file(&bin.path("trust.json"), &secret);
        let e = open_nofollow(&bin.path("trust.json")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        assert!(bin.read::<serde_json::Value>("trust.json").is_err());
        symlink_file(&bin.path(".lock"), &secret);
        assert!(bin.lock().is_err());
        // A link planted as the temporary file: the write replaces the
        // link (or fails), and the file it points at is untouched.
        symlink_file(&bin.path(".updater.json.tmp"), &secret);
        let _ = bin.write("updater.json", &serde_json::json!({"n": 1}));
        assert_eq!(fs::read(&secret).unwrap(), br#"{"format":1}"#);
        // A link as the target itself is replaced, not written through.
        let other = outside.path().join("other.json");
        fs::write(&other, b"untouched").unwrap();
        symlink_file(&bin.path("skipped"), &other);
        bin.write("skipped", &serde_json::json!([])).unwrap();
        assert_eq!(fs::read(&other).unwrap(), b"untouched");
        let back: serde_json::Value = bin.read("skipped").unwrap().unwrap();
        assert_eq!(back, serde_json::json!([]));
    }

    #[test]
    fn writes_replace_atomically() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        bin.write("updater.json", &serde_json::json!({"n": 1}))
            .unwrap();
        bin.write_acknowledged("updater.json", &serde_json::json!({"n": 2}), &|_| Ok(()))
            .unwrap();
        let back: serde_json::Value = bin.read("updater.json").unwrap().unwrap();
        assert_eq!(back["n"], 2);
        // A write that fails before the rename leaves the old file whole.
        let failed = bin.write_acknowledged("updater.json", &serde_json::json!({"n": 3}), &|s| {
            if s == Step::Rename {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        });
        assert!(failed.is_err());
        let back: serde_json::Value = bin.read("updater.json").unwrap().unwrap();
        assert_eq!(back["n"], 2);
        let names: Vec<_> = fs::read_dir(bin.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    #[test]
    fn a_second_writer_is_blocked() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let other = Bin::open(data.path()).unwrap();
        let held = bin.lock().unwrap().unwrap();
        assert!(other.lock().unwrap().is_none());
        drop(held);
        assert!(other.lock().unwrap().is_some());
    }

    /// While a `Bin` lives, its folder can't be renamed away (and a
    /// junction put in its place).
    #[test]
    fn the_folder_is_pinned_while_open() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        assert!(fs::rename(bin.dir(), data.path().join("moved")).is_err());
        let dir = bin.dir().to_path_buf();
        drop(bin);
        fs::rename(&dir, data.path().join("moved")).unwrap();
    }

    #[test]
    fn an_oversize_file_is_damage() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        fs::write(bin.path("active"), vec![b' '; MAX_STATE as usize + 1]).unwrap();
        let e = bin.read::<serde_json::Value>("active").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn a_retry_is_bounded_and_returns_the_last_error() {
        let mut n = 0;
        let e = retry(
            |_| true,
            || {
                n += 1;
                Err(io::Error::from_raw_os_error(32))
            },
        )
        .unwrap_err();
        assert_eq!((n, e.raw_os_error()), (RETRY_ATTEMPTS, Some(32)));
        let mut n = 0;
        let _ = retry(
            |e| e.raw_os_error() == Some(32),
            || {
                n += 1;
                Err(io::Error::from(io::ErrorKind::NotFound))
            },
        );
        assert_eq!(n, 1, "anything else comes back at once");
    }
}
