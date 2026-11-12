//! The watch folder (UP-1, `docs/decisions/2026-11-12-UP-1.md`, plan
//! §4.3): `.torrent` files dropped into one folder are taken exactly as
//! uploads are, through the pipeline's `local_torrent` (the same read,
//! the same judgement, the same commit), then moved to `added/` or
//! `failed/` inside the folder, or deleted, so they're never read again.
//!
//! - **Polling, not filesystem events,** every [`POLL_EVERY`]: works on
//!   Docker bind mounts and SMB. Each poll keeps at most [`MAX_SCAN`]
//!   `.torrent` names (among at most [`MAX_LIST`] entries) and submits at
//!   most [`MAX_SUBMIT`] files, in name order, resuming after the last
//!   file taken. The folder and the data-directory rule are checked again
//!   on every poll; a problem pauses the watch until it's fixed.
//! - **Only regular `.torrent` files directly in the folder.** Nothing is
//!   followed: not a symbolic link, a junction or any other reparse
//!   point, the folder's own `added/` and `failed/` included. A file is
//!   checked through its open handle, not its name. On Unix every step
//!   that acts on a file (open, mkdir, rename, unlink) goes through the
//!   folder's own handle, opened without following, and the file is
//!   opened `O_NOFOLLOW`; the listing and each entry's type are only
//!   hints. On Windows a file is opened as a reparse point itself and
//!   refused if it is one. A file over `local_torrent::MAX_BYTES` is
//!   never read.
//! - **A file is read once it's complete:** its size and modification
//!   time the same on two polls in a row, and not held open by another
//!   program (skipped, never failed).
//! - **Never read twice.** A file that can't be moved or deleted yet is
//!   remembered in a bounded set, never read again, and its move retried
//!   quietly each poll; when the set is full the watch holds.
//! - **No file name in any log line or status**, only counts and causes:
//!   names in a user's folder are often torrent titles.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lodestar_log::{Count, log_event};
use lodestar_pipeline::local_torrent::{Closed, MAX_BYTES, Outcome, Uploads};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::settings::watch_folder::{After, FolderProblem, Watch, folder_problem, is_link};

/// How often the folder is looked at: 10 s (≥ 5 s, plan §4.3; a §2.26
/// number). A file is read on the second poll that sees it unchanged, so
/// about 10–20 s after it's complete.
pub const POLL_EVERY: Duration = Duration::from_secs(10);
/// Directory entries looked at per poll, at most (a §2.26 number): enough
/// for any folder a person fills by hand, while a folder of a million
/// entries costs one bounded listing per poll, not a stall.
pub const MAX_SCAN: usize = if cfg!(test) { 64 } else { 10_000 };
/// Directory entries read per poll, at most, `.torrent` or not (a §2.26
/// number): a bound on the listing's cost, high enough that other files
/// left in the folder don't hide new torrents.
pub const MAX_LIST: usize = if cfg!(test) { 200 } else { 200_000 };
/// Files read and submitted per poll, at most (a §2.26 number): 32 every
/// 10 s is about 190 a minute, more than the judge's share of the
/// pipeline should take from the crawl, and one file is held at a time.
pub const MAX_SUBMIT: usize = 32;
/// Files that couldn't be moved or deleted, remembered so they're never
/// read again (a §2.26 number). When it's full the watch stops. Tests use
/// 4, so filling it takes a few files.
pub const MAX_STUCK: usize = if cfg!(test) { 4 } else { 1024 };
/// Numbered names tried for a clash in `added/` or `failed/`.
const MAX_SUFFIX: u32 = 1000;

pub const ADDED: &str = "added";
pub const FAILED: &str = "failed";

lodestar_log::catalogue! {
    "lodestar-watch";
    /// The watch folder is being watched (the setting was applied, at
    /// start or when it changed).
    WATCH_STARTED {
        id: "watch.started",
        component: Sources,
        level: Info,
        message: "Watching a folder for .torrent files",
        fields: {},
    }
    /// The watch folder setting was cleared.
    WATCH_STOPPED {
        id: "watch.stopped",
        component: Sources,
        level: Info,
        message: "Stopped watching the folder",
        fields: {},
    }
    /// The watch can't run, and why (a fixed cause, never the path):
    /// `not_absolute`, `missing`, `not_directory`, `link`,
    /// `inside_data_dir`, `holds_data_dir`, `subfolder_link`,
    /// `subfolder_not_directory`, `subfolder_create_failed`,
    /// `unreadable` or `too_many_stuck`.
    WATCH_DISABLED {
        id: "watch.disabled",
        component: Sources,
        level: Warn,
        message: "The watch folder is off until its setting is fixed",
        fields: { reason: Label },
    }
    /// One poll took files: how many went to `added/` (or were deleted),
    /// how many to `failed/`, and of those how many because judging
    /// failed on Lodestar's side (`errors`), not because of the file.
    WATCH_TOOK {
        id: "watch.took",
        component: Sources,
        level: Info,
        message: "Took .torrent files from the watch folder",
        fields: { added: Label, failed: Label, errors: Label },
    }
    /// A file couldn't be moved or deleted after it was taken (a
    /// read-only folder, say). It won't be read again; `stuck` is how
    /// many are remembered now. Once per file, never per poll.
    WATCH_STUCK {
        id: "watch.stuck",
        component: Sources,
        level: Warn,
        message: "Couldn't move a file out of the watch folder; it won't be read again",
        fields: { stuck: Label },
    }
}

/// Why the watch isn't running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    Folder(FolderProblem),
    /// `added/` or `failed/` is a link or other reparse point.
    SubfolderLink,
    /// `added/` or `failed/` is there but isn't a directory.
    SubfolderNotDirectory,
    /// `added/` or `failed/` couldn't be created.
    SubfolderCreate,
    /// The folder can't be listed.
    Unreadable,
    /// [`MAX_STUCK`] files couldn't be moved.
    TooManyStuck,
}

impl Problem {
    pub fn code(self) -> &'static str {
        match self {
            Problem::Folder(p) => p.code(),
            Problem::SubfolderLink => "subfolder_link",
            Problem::SubfolderNotDirectory => "subfolder_not_directory",
            Problem::SubfolderCreate => "subfolder_create_failed",
            Problem::Unreadable => "unreadable",
            Problem::TooManyStuck => "too_many_stuck",
        }
    }
}

/// Why a file went to `failed/`: its name there is `<stem>.<cause>.torrent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Malformed,
    Refused,
    Held,
    Failed,
    TooLarge,
    /// A link or reparse point in place of a file.
    Link,
    /// A Unix hard link: the same file is also somewhere else. Windows
    /// has no link count on stable Rust, so it's never built there.
    #[cfg_attr(not(unix), allow(dead_code))]
    Linked,
    Unreadable,
}

impl Cause {
    fn code(self) -> &'static str {
        match self {
            Cause::Malformed => "malformed",
            Cause::Refused => "refused",
            Cause::Held => "held",
            Cause::Failed => "failed",
            Cause::TooLarge => "too_large",
            Cause::Link => "link",
            Cause::Linked => "linked",
            Cause::Unreadable => "unreadable",
        }
    }
}

/// Where files go to be judged: the pipeline's uploads, or a stand-in.
pub trait Sink: Send + Sync {
    fn submit(
        &self,
        body: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<Outcome, Closed>> + Send;
}

impl Sink for Uploads {
    async fn submit(&self, body: Vec<u8>) -> Result<Outcome, Closed> {
        Uploads::submit(self, body).await
    }
}

/// What one poll did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Polled {
    pub added: u64,
    pub failed: u64,
    /// Of `failed`, files whose judging panicked (`failed/<stem>.failed`):
    /// a fault on Lodestar's side, so counted apart from a bad file.
    pub errors: u64,
    /// Files seen but not read yet: still changing, held open, or past
    /// this poll's [`MAX_SUBMIT`].
    pub waiting: u64,
    /// Why the watch isn't running now, if it isn't: checked again every
    /// poll, so a mount that comes back or a fixed folder resumes it,
    /// except [`Problem::TooManyStuck`], which holds until the setting
    /// changes or a restart.
    pub problem: Option<Problem>,
    /// The pipeline isn't taking uploads: the rest waits for the next
    /// poll.
    pub closed: bool,
}

/// A file's size and modification time, as two polls compare them.
type Shape = (u64, Option<SystemTime>);

/// A file taken but not moved or deleted yet (a read-only folder, say,
/// or one another program still holds): never read again, and its move
/// retried quietly on every poll.
#[derive(Debug, Clone, Copy)]
struct Stuck {
    /// Its shape when it was taken; `None` for a link.
    shape: Option<Shape>,
    cause: Option<Cause>,
}

/// One watch folder's state between polls.
#[derive(Debug)]
pub struct Watcher {
    folder: PathBuf,
    data_dir: PathBuf,
    after: After,
    /// Each candidate's shape on the last poll.
    seen: HashMap<OsString, Shape>,
    /// The last file taken: the next poll starts after it.
    cursor: Option<OsString>,
    /// Files taken but not moved yet, by name.
    stuck: HashMap<OsString, Stuck>,
    /// [`MAX_STUCK`] were: the watch holds.
    halted: bool,
}

impl Watcher {
    /// A watcher for `settings`; `None` when the setting is off. Whether
    /// the folder can be watched is checked on every poll.
    pub fn new(settings: &Watch, data_dir: &Path) -> Option<Watcher> {
        Some(Watcher {
            folder: settings.folder()?,
            data_dir: data_dir.to_path_buf(),
            after: settings.after,
            seen: HashMap::new(),
            cursor: None,
            stuck: HashMap::new(),
            halted: false,
        })
    }

    /// Why the folder can't be watched now, if it can't.
    pub fn check(&self) -> Option<Problem> {
        folder_problem(&self.folder, &self.data_dir).map(Problem::Folder)
    }

    /// Takes over `before`'s memory when it watched the same folder, so a
    /// change to `after` alone never reads a stuck file again.
    fn inherit(&mut self, before: Watcher) {
        if before.folder == self.folder {
            self.seen = before.seen;
            self.cursor = before.cursor;
            self.stuck = before.stuck;
            self.halted = before.halted;
        }
    }

    /// One poll: check the folder, scan it, then take each file that's
    /// ready, in name order after the last one taken, up to
    /// [`MAX_SUBMIT`]. Blocking file work runs on the blocking pool.
    pub async fn poll<S: Sink>(&mut self, sink: &S) -> Polled {
        let mut out = Polled::default();
        if self.halted {
            out.problem = Some(Problem::TooManyStuck);
            return out;
        }
        let (path, data) = (self.folder.clone(), self.data_dir.clone());
        let cursor = self.cursor.clone();
        // Checked, listed and looked at in one go on the blocking pool, in
        // the order they'll be taken.
        let opened = blocking(move || {
            Folder::open(&path, &data).and_then(|f| {
                let names = f.list().map_err(|_| Problem::Unreadable)?;
                let entries: Vec<_> = resume_order(names, cursor.as_deref())
                    .into_iter()
                    .filter_map(|n| f.entry(&n).map(|e| (n, e)))
                    .collect();
                Ok((entries, f))
            })
        })
        .await;
        let (entries, folder) = match opened {
            Ok((entries, f)) => (entries, std::sync::Arc::new(f)),
            Err(p) => {
                out.problem = Some(p);
                return out;
            }
        };
        let mut seen = HashMap::new();
        let mut submitted = 0;
        for (name, entry) in entries {
            if self.retry_stuck(&folder, &name, &entry).await {
                continue;
            }
            let shape = match entry {
                Entry::Other => continue,
                Entry::Link => {
                    self.finish(&folder, &name, None, Some(Cause::Link), &mut out)
                        .await;
                    continue;
                }
                Entry::File(shape) => shape,
            };
            if shape.0 > MAX_BYTES as u64 {
                // Never read.
                self.finish(&folder, &name, Some(shape), Some(Cause::TooLarge), &mut out)
                    .await;
                continue;
            }
            seen.insert(name.clone(), shape);
            if self.seen.get(&name) != Some(&shape) || submitted == MAX_SUBMIT {
                out.waiting += 1;
                continue;
            }
            let f = folder.clone();
            let n = name.clone();
            let read = blocking(move || f.read(&n, shape)).await;
            let body = match read {
                Taken::Body(body) => body,
                Taken::Wait => {
                    out.waiting += 1;
                    continue;
                }
                Taken::Refuse(cause) => {
                    self.finish(&folder, &name, Some(shape), Some(cause), &mut out)
                        .await;
                    continue;
                }
            };
            submitted += 1;
            let outcome = match sink.submit(body).await {
                Ok(o) => o,
                Err(Closed) => {
                    out.closed = true;
                    break;
                }
            };
            let cause = match outcome {
                Outcome::Accepted { .. } => None,
                Outcome::Malformed => Some(Cause::Malformed),
                Outcome::Refused => Some(Cause::Refused),
                Outcome::Held => Some(Cause::Held),
                Outcome::Failed => Some(Cause::Failed),
            };
            self.cursor = Some(name.clone());
            self.finish(&folder, &name, Some(shape), cause, &mut out)
                .await;
            if self.halted {
                break;
            }
        }
        self.seen = seen;
        if self.halted {
            out.problem = Some(Problem::TooManyStuck);
        }
        out
    }

    /// For a file taken before but not moved: retries its move quietly
    /// (no count, no log) and says it's handled. A file that changed since
    /// is a new file: forgotten here, and judged afresh. One still over
    /// the cap stays stuck under its name, however it grows.
    async fn retry_stuck(
        &mut self,
        folder: &std::sync::Arc<Folder>,
        name: &OsStr,
        entry: &Entry,
    ) -> bool {
        let Some(stuck) = self.stuck.get(name).copied() else {
            return false;
        };
        let same = match (entry, stuck.shape) {
            (Entry::Link, None) => true,
            (Entry::File(now), Some(then)) => {
                *now == then || (stuck.cause == Some(Cause::TooLarge) && now.0 > MAX_BYTES as u64)
            }
            _ => false,
        };
        if !same {
            self.stuck.remove(name);
            return false;
        }
        if self.put_away(folder, name, stuck.cause).await.is_ok() {
            self.stuck.remove(name);
        }
        true
    }

    /// Moves a taken file to `added/` (or deletes it, in delete mode) or
    /// to `failed/` with its cause in its name.
    async fn put_away(
        &self,
        folder: &std::sync::Arc<Folder>,
        name: &OsStr,
        cause: Option<Cause>,
    ) -> Result<(), MoveError> {
        let delete = cause.is_none() && self.after == After::Delete;
        let f = folder.clone();
        let n = name.to_os_string();
        blocking(move || {
            if delete {
                f.remove(&n)
            } else {
                f.move_out(&n, cause)
            }
        })
        .await
    }

    /// Puts a taken file away and counts it. One that can't be moved or
    /// deleted is remembered, never read again, and reported once; when
    /// [`MAX_STUCK`] are, the watch holds.
    async fn finish(
        &mut self,
        folder: &std::sync::Arc<Folder>,
        name: &OsStr,
        shape: Option<Shape>,
        cause: Option<Cause>,
        out: &mut Polled,
    ) {
        match cause {
            None => out.added += 1,
            Some(Cause::Failed) => {
                out.failed += 1;
                out.errors += 1;
            }
            Some(_) => out.failed += 1,
        }
        match self.put_away(folder, name, cause).await {
            Ok(()) => {}
            // Checked again next poll; the file stays where it is.
            Err(MoveError::Subfolder(p)) => {
                self.stuck
                    .insert(name.to_os_string(), Stuck { shape, cause });
                out.problem = Some(p);
            }
            Err(MoveError::Io) => {
                if self.stuck.len() >= MAX_STUCK {
                    self.halted = true;
                    return;
                }
                self.stuck
                    .insert(name.to_os_string(), Stuck { shape, cause });
                log_event!(WATCH_STUCK, stuck = Count(self.stuck.len() as u64));
                if self.stuck.len() >= MAX_STUCK {
                    self.halted = true;
                }
            }
        }
    }
}

/// The names after `cursor` first, then the rest: every name gets its
/// turn, in order, however many polls a big folder takes.
fn resume_order(mut names: Vec<OsString>, cursor: Option<&OsStr>) -> Vec<OsString> {
    names.sort();
    let Some(cursor) = cursor else {
        return names;
    };
    let split = names.partition_point(|n| n.as_os_str() <= cursor);
    names.rotate_left(split);
    names
}

/// A `.torrent` name, by ASCII case-insensitive extension.
fn is_torrent_name(name: &OsStr) -> bool {
    let b = name.as_encoded_bytes();
    b.len() > ".torrent".len() && b[b.len() - ".torrent".len()..].eq_ignore_ascii_case(b".torrent")
}

/// `name` in `failed/` with its cause, or as it is in `added/`, numbered
/// on a clash: `<stem>.<cause>.torrent`, then `<stem>.<cause>-1.torrent`.
fn target_name(name: &OsStr, cause: Option<Cause>, n: u32) -> OsString {
    let b = name.as_encoded_bytes();
    let stem = &b[..b.len() - ".torrent".len()];
    let mut out = stem.to_vec();
    if let Some(c) = cause {
        out.push(b'.');
        out.extend_from_slice(c.code().as_bytes());
    }
    if n > 0 {
        out.extend_from_slice(format!("-{n}").as_bytes());
    }
    out.extend_from_slice(&b[b.len() - ".torrent".len()..]);
    os_string(out)
}

/// Bytes from an `OsStr`'s encoding, split only at ASCII and joined with
/// ASCII, back into an `OsString`.
fn os_string(bytes: Vec<u8>) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(bytes)
    }
    #[cfg(not(unix))]
    {
        // Without `unsafe` (the workspace forbids it) there's no checked
        // way back from WTF-8, so a name that isn't valid Unicode gets a
        // replacement character in its new name only; the file it names
        // is still moved by its own name.
        OsString::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Runs `f` on the blocking pool.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("a watch folder task panicked")
}

/// A listed entry, read without following it.
enum Entry {
    File(Shape),
    Link,
    Other,
}

enum Taken {
    Body(Vec<u8>),
    /// Held open by another program, or changed since it was looked at.
    Wait,
    Refuse(Cause),
}

enum MoveError {
    Subfolder(Problem),
    Io,
}

/// The folder for one poll, opened without following links, with its
/// `added/` and `failed/` checked.
struct Folder {
    path: PathBuf,
    #[cfg(unix)]
    fd: rustix::fd::OwnedFd,
}

impl Folder {
    /// The folder as it is now: checked again every poll (its shape and
    /// the data-directory rule, so a folder swapped for a link, or for one
    /// inside the data directory, is never read), then opened without
    /// following it and its `added/` and `failed/` checked.
    fn open(path: &Path, data_dir: &Path) -> Result<Folder, Problem> {
        if let Some(p) = folder_problem(path, data_dir) {
            return Err(Problem::Folder(p));
        }
        #[cfg(unix)]
        let fd = {
            use rustix::fs::{Mode, OFlags};
            rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| match e {
                rustix::io::Errno::LOOP => Problem::Folder(FolderProblem::Link),
                rustix::io::Errno::NOENT => Problem::Folder(FolderProblem::Missing),
                rustix::io::Errno::NOTDIR => Problem::Folder(FolderProblem::NotDirectory),
                _ => Problem::Unreadable,
            })?
        };
        let f = Folder {
            path: path.to_path_buf(),
            #[cfg(unix)]
            fd,
        };
        f.subfolder(ADDED)?;
        f.subfolder(FAILED)?;
        Ok(f)
    }

    /// Checks `name` inside the folder is a real directory, creating it
    /// when it's missing. A link there stops the watch.
    fn subfolder(&self, name: &str) -> Result<(), Problem> {
        let path = self.path.join(name);
        let check = |meta: &std::fs::Metadata| {
            if is_link(meta) {
                Err(Problem::SubfolderLink)
            } else if !meta.is_dir() {
                Err(Problem::SubfolderNotDirectory)
            } else {
                Ok(())
            }
        };
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => check(&meta),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(unix)]
                let made = rustix::fs::mkdirat(&self.fd, name, rustix::fs::Mode::from(0o777))
                    .map_err(std::io::Error::from);
                #[cfg(not(unix))]
                let made = std::fs::create_dir(&path);
                match made {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(Problem::SubfolderCreate),
                }
                let meta =
                    std::fs::symlink_metadata(&path).map_err(|_| Problem::SubfolderCreate)?;
                check(&meta)
            }
            Err(_) => Err(Problem::SubfolderCreate),
        }
    }

    /// The `.torrent` names directly in the folder: at most [`MAX_SCAN`]
    /// of them, among at most [`MAX_LIST`] entries read. Only `.torrent`
    /// names count against the first, so other files and folders that stay
    /// there can't hide a new torrent.
    fn list(&self) -> std::io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.path)?.take(MAX_LIST) {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            if is_torrent_name(&name) {
                names.push(name);
                if names.len() == MAX_SCAN {
                    break;
                }
            }
        }
        Ok(names)
    }

    /// What `name` is, read without following it.
    fn entry(&self, name: &OsStr) -> Option<Entry> {
        let meta = std::fs::symlink_metadata(self.path.join(name)).ok()?;
        Some(if is_link(&meta) {
            Entry::Link
        } else if meta.is_file() {
            Entry::File((meta.len(), meta.modified().ok()))
        } else {
            Entry::Other
        })
    }

    /// Opens `name` without following it and reads it, checked through
    /// the handle: a regular file, the shape it was listed with, one link
    /// to it (Unix), and within the cap, read through a limit.
    fn read(&self, name: &OsStr, shape: Shape) -> Taken {
        let file = match self.open_file(name) {
            Ok(f) => f,
            Err(e) => return open_error(&e),
        };
        let Ok(meta) = file.metadata() else {
            return Taken::Refuse(Cause::Unreadable);
        };
        #[cfg(windows)]
        if is_link(&meta) {
            return Taken::Refuse(Cause::Link);
        }
        if !meta.is_file() {
            return Taken::Wait;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() > 1 {
                return Taken::Refuse(Cause::Linked);
            }
        }
        if meta.len() > MAX_BYTES as u64 {
            return Taken::Refuse(Cause::TooLarge);
        }
        if (meta.len(), meta.modified().ok()) != shape {
            return Taken::Wait;
        }
        let mut body = Vec::with_capacity(meta.len() as usize);
        match file.take(MAX_BYTES as u64 + 1).read_to_end(&mut body) {
            Ok(_) if body.len() > MAX_BYTES => Taken::Refuse(Cause::TooLarge),
            Ok(_) => Taken::Body(body),
            Err(e) if busy(&e) => Taken::Wait,
            Err(_) => Taken::Refuse(Cause::Unreadable),
        }
    }

    #[cfg(unix)]
    fn open_file(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        use rustix::fs::{Mode, OFlags};
        let fd = rustix::fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(std::fs::File::from(fd))
    }

    #[cfg(windows)]
    fn open_file(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        // The link itself, not its target; refused by its attribute.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.path.join(name))
    }

    /// Moves `name` into `added/` (no cause) or `failed/`, never over a
    /// file already there.
    fn move_out(&self, name: &OsStr, cause: Option<Cause>) -> Result<(), MoveError> {
        let sub = if cause.is_some() { FAILED } else { ADDED };
        self.subfolder(sub).map_err(MoveError::Subfolder)?;
        for n in 0..MAX_SUFFIX {
            let target = target_name(name, cause, n);
            match self.rename_no_replace(name, sub, &target) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(MoveError::Io),
            }
        }
        Err(MoveError::Io)
    }

    #[cfg(unix)]
    fn rename_no_replace(&self, name: &OsStr, sub: &str, target: &OsStr) -> std::io::Result<()> {
        use rustix::fs::{AtFlags, Mode, OFlags};
        let dir = rustix::fs::openat(
            &self.fd,
            sub,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        {
            match rustix::fs::renameat_with(
                &self.fd,
                name,
                &dir,
                target,
                rustix::fs::RenameFlags::NOREPLACE,
            ) {
                Ok(()) => return Ok(()),
                Err(e) if e == rustix::io::Errno::EXIST => {
                    return Err(std::io::ErrorKind::AlreadyExists.into());
                }
                // A filesystem without the flag (some network mounts):
                // checked, then renamed.
                Err(e)
                    if e == rustix::io::Errno::INVAL
                        || e == rustix::io::Errno::NOSYS
                        || e == rustix::io::Errno::NOTSUP => {}
                Err(e) => return Err(e.into()),
            }
        }
        if rustix::fs::statat(&dir, target, AtFlags::SYMLINK_NOFOLLOW).is_ok() {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        rustix::fs::renameat(&self.fd, name, &dir, target)?;
        Ok(())
    }

    /// A hard link at the target, which fails if anything is there, then
    /// the original removed: never over a file (`std::fs::rename` would
    /// replace one, so it's never used). Where hard links aren't there
    /// (FAT, exFAT, some shares) the move fails, and the file joins the
    /// never-read-again set (runner, 2026-11-12). A link entry isn't
    /// moved at all: a hard link to a link isn't defined the same way
    /// everywhere, so it stays where it is, never read. Should Lodestar
    /// stop between the link and the removal, both names are left and the
    /// next poll may submit the file again: harmless, the store keeps one
    /// torrent per info hash.
    #[cfg(windows)]
    fn rename_no_replace(&self, name: &OsStr, sub: &str, target: &OsStr) -> std::io::Result<()> {
        let from = self.path.join(name);
        if std::fs::symlink_metadata(&from).is_ok_and(|m| is_link(&m)) {
            return Err(std::io::ErrorKind::Unsupported.into());
        }
        std::fs::hard_link(&from, self.path.join(sub).join(target))?;
        std::fs::remove_file(&from)
    }

    fn remove(&self, name: &OsStr) -> Result<(), MoveError> {
        #[cfg(unix)]
        let done = rustix::fs::unlinkat(&self.fd, name, rustix::fs::AtFlags::empty())
            .map_err(std::io::Error::from);
        #[cfg(windows)]
        let done = std::fs::remove_file(self.path.join(name));
        done.map_err(|_| MoveError::Io)
    }
}

/// What a failed open means for the file.
fn open_error(e: &std::io::Error) -> Taken {
    if busy(e) || e.kind() == std::io::ErrorKind::NotFound {
        return Taken::Wait;
    }
    #[cfg(unix)]
    if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
        // Swapped for a link since it was listed.
        return Taken::Refuse(Cause::Link);
    }
    Taken::Refuse(Cause::Unreadable)
}

/// Held by another program: try again on a later poll, never a failure.
fn busy(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(windows)]
    {
        // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
        if matches!(e.raw_os_error(), Some(32 | 33)) {
            return true;
        }
    }
    #[cfg(unix)]
    {
        let busy = [rustix::io::Errno::BUSY, rustix::io::Errno::TXTBSY];
        if busy
            .iter()
            .any(|b| e.raw_os_error() == Some(b.raw_os_error()))
        {
            return true;
        }
    }
    false
}

/// Follows the `watch` section: applies it at once and whenever it
/// changes, and polls every [`POLL_EVERY`] while it's on. A problem is
/// logged when it starts and the recovery when it ends, never per poll.
/// Ends when the registry goes away.
pub fn follow(
    mut rx: watch::Receiver<Watch>,
    data_dir: PathBuf,
    uploads: Uploads,
) -> JoinHandle<()> {
    let first = rx.borrow_and_update().clone();
    tokio::spawn(async move {
        let mut shown = None;
        let mut watcher = apply(&first, &data_dir, None, &mut shown);
        let mut tick = tokio::time::interval(POLL_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let now = rx.borrow_and_update().clone();
                    watcher = apply(&now, &data_dir, watcher.take(), &mut shown);
                }
                _ = tick.tick() => {
                    let Some(w) = watcher.as_mut() else { continue };
                    let polled = w.poll(&uploads).await;
                    if polled.added + polled.failed > 0 {
                        log_event!(
                            WATCH_TOOK,
                            added = Count(polled.added),
                            failed = Count(polled.failed),
                            errors = Count(polled.errors)
                        );
                    }
                    show(polled.problem, &mut shown);
                }
            }
        }
    })
}

/// Logs a change of problem: `watch.disabled` when one starts or
/// changes, `watch.started` when it clears.
fn show(problem: Option<Problem>, shown: &mut Option<Option<Problem>>) {
    if *shown == Some(problem) {
        return;
    }
    match problem {
        Some(p) => log_event!(WATCH_DISABLED, reason = p.code()),
        None => log_event!(WATCH_STARTED),
    }
    *shown = Some(problem);
}

/// The watcher for `settings`, keeping `before`'s memory of the same
/// folder, with the change logged.
fn apply(
    settings: &Watch,
    data_dir: &Path,
    before: Option<Watcher>,
    shown: &mut Option<Option<Problem>>,
) -> Option<Watcher> {
    let Some(mut w) = Watcher::new(settings, data_dir) else {
        if before.is_some() {
            log_event!(WATCH_STOPPED);
        }
        *shown = None;
        return None;
    };
    if let Some(b) = before {
        w.inherit(b);
    }
    *shown = None;
    show(w.check(), shown);
    Some(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Stands in for the pipeline: answers by a marker in the body, and
    /// counts what reached it.
    #[derive(Default)]
    struct Fake {
        calls: AtomicUsize,
        bodies: Mutex<Vec<Vec<u8>>>,
    }

    impl Sink for Fake {
        async fn submit(&self, body: Vec<u8>) -> Result<Outcome, Closed> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let has = |m: &[u8]| body.windows(m.len()).any(|w| w == m);
            let o = if has(b"HELD") {
                Outcome::Held
            } else if has(b"MALF") {
                Outcome::Malformed
            } else if has(b"FLOOR") {
                Outcome::Refused
            } else if has(b"PANIC") {
                Outcome::Failed
            } else if has(b"CLOSED") {
                return Err(Closed);
            } else {
                Outcome::Accepted {
                    key: lodestar_metainfo::InfoHash([1; 20]),
                    private: false,
                }
            };
            self.bodies.lock().unwrap().push(body);
            Ok(o)
        }
    }

    fn calls(f: &Fake) -> usize {
        f.calls.load(Ordering::SeqCst)
    }

    struct T {
        root: tempfile::TempDir,
        data: tempfile::TempDir,
    }

    impl T {
        fn new() -> T {
            T {
                root: tempfile::tempdir().unwrap(),
                data: tempfile::tempdir().unwrap(),
            }
        }
        fn dir(&self) -> &Path {
            self.root.path()
        }
        fn put(&self, name: &str, body: &[u8]) {
            std::fs::write(self.dir().join(name), body).unwrap();
        }
        fn watcher(&self, after: After) -> Watcher {
            let w = Watch {
                folder: self.dir().to_string_lossy().into_owned(),
                after,
            };
            Watcher::new(&w, self.data.path()).unwrap()
        }
        fn names(&self, sub: &str) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(self.dir().join(sub))
                .map(|r| {
                    r.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        }
        fn top(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(self.dir())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != ADDED && n != FAILED)
                .collect();
            v.sort();
            v
        }
    }

    /// Two polls: the first sees each file, the second takes it.
    async fn settle(w: &mut Watcher, f: &Fake) -> Polled {
        let first = w.poll(f).await;
        assert_eq!(
            first.added + first.failed,
            0,
            "nothing is read on first sight"
        );
        w.poll(f).await
    }

    #[tokio::test]
    async fn an_accepted_file_goes_to_added() {
        let t = T::new();
        t.put("Sintel.torrent", b"d4:infodee");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        let p = settle(&mut w, &f).await;
        assert_eq!((p.added, p.failed), (1, 0));
        assert_eq!(t.names(ADDED), ["Sintel.torrent"]);
        assert!(t.top().is_empty());
        assert_eq!(f.bodies.lock().unwrap()[0], b"d4:infodee");
        // Never read again.
        w.poll(&f).await;
        assert_eq!(calls(&f), 1);
    }

    #[tokio::test]
    async fn every_other_outcome_goes_to_failed_with_its_cause() {
        let t = T::new();
        t.put("a.torrent", b"MALF");
        t.put("b.torrent", b"HELD");
        t.put("c.torrent", b"FLOOR");
        t.put("d.torrent", b"PANIC");
        t.put("e.torrent", b"fine");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        let p = settle(&mut w, &f).await;
        assert_eq!((p.added, p.failed, p.errors), (1, 4, 1));
        assert_eq!(
            t.names(FAILED),
            [
                "a.malformed.torrent",
                "b.held.torrent",
                "c.refused.torrent",
                "d.failed.torrent"
            ]
        );
        // rv-753 N3: the file after the panicking one was still taken.
        assert_eq!(t.names(ADDED), ["e.torrent"]);
    }

    #[tokio::test]
    async fn an_oversize_file_is_failed_without_being_read() {
        let t = T::new();
        let big = t.dir().join("big.torrent");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_BYTES as u64 + 1)
            .unwrap();
        // Unreadable too, so a read would show as `unreadable`, not
        // `too_large` (where permissions bind: not for root).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        // On first sight: size alone decides.
        let p = w.poll(&f).await;
        assert_eq!(p.failed, 1);
        assert_eq!(t.names(FAILED), ["big.too_large.torrent"]);
        assert_eq!(calls(&f), 0);
    }

    #[tokio::test]
    async fn a_growing_file_waits_until_it_is_still() {
        let t = T::new();
        t.put("grow.torrent", b"d4:info");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        assert_eq!(w.poll(&f).await.waiting, 1);
        // It grows between polls: seen again, not read.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(t.dir().join("grow.torrent"))
            .unwrap();
        std::io::Write::write_all(&mut file, b"de").unwrap();
        drop(file);
        assert_eq!(w.poll(&f).await.waiting, 1);
        assert_eq!(calls(&f), 0);
        // Unchanged since: read.
        assert_eq!(w.poll(&f).await.added, 1);
        assert_eq!(f.bodies.lock().unwrap()[0], b"d4:infode");
    }

    #[tokio::test]
    async fn other_names_and_folders_are_left_alone() {
        let t = T::new();
        t.put("notes.txt", b"x");
        t.put("a.torrent.part", b"x");
        t.put(".torrent", b"x");
        std::fs::create_dir(t.dir().join("dir.torrent")).unwrap();
        std::fs::create_dir(t.dir().join("nested")).unwrap();
        std::fs::write(t.dir().join("nested").join("inner.torrent"), b"x").unwrap();
        t.put("UPPER.TORRENT", b"fine");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        let p = settle(&mut w, &f).await;
        assert_eq!((p.added, p.failed), (1, 0));
        assert_eq!(t.names(ADDED), ["UPPER.TORRENT"]);
        assert_eq!(
            t.top(),
            [
                ".torrent",
                "a.torrent.part",
                "dir.torrent",
                "nested",
                "notes.txt"
            ]
        );
        assert_eq!(t.names("nested"), ["inner.torrent"]);
    }

    #[tokio::test]
    async fn a_name_clash_gets_a_number() {
        let t = T::new();
        std::fs::create_dir(t.dir().join(ADDED)).unwrap();
        std::fs::write(t.dir().join(ADDED).join("x.torrent"), b"old").unwrap();
        std::fs::write(t.dir().join(ADDED).join("x-1.torrent"), b"old").unwrap();
        t.put("x.torrent", b"new");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        settle(&mut w, &f).await;
        assert_eq!(t.names(ADDED), ["x-1.torrent", "x-2.torrent", "x.torrent"]);
        assert_eq!(
            std::fs::read(t.dir().join(ADDED).join("x.torrent")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(t.dir().join(ADDED).join("x-2.torrent")).unwrap(),
            b"new"
        );
    }

    #[tokio::test]
    async fn delete_mode_deletes_what_was_added_and_keeps_what_failed() {
        let t = T::new();
        t.put("good.torrent", b"fine");
        t.put("bad.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Delete);
        let p = settle(&mut w, &f).await;
        assert_eq!((p.added, p.failed), (1, 1));
        assert!(t.names(ADDED).is_empty());
        assert_eq!(t.names(FAILED), ["bad.malformed.torrent"]);
        assert!(t.top().is_empty());
    }

    #[tokio::test]
    async fn each_poll_is_bounded_and_the_next_resumes_in_order() {
        let t = T::new();
        let n = MAX_SUBMIT + 8;
        for i in 0..n {
            t.put(&format!("{i:03}.torrent"), b"fine");
        }
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        let p = settle(&mut w, &f).await;
        assert_eq!(p.added as usize, MAX_SUBMIT);
        assert_eq!(p.waiting, 8);
        let first: Vec<String> = (0..MAX_SUBMIT).map(|i| format!("{i:03}.torrent")).collect();
        assert_eq!(t.names(ADDED), first);
        // A file that sorts before the cursor arrives meanwhile: the rest
        // go first, then it.
        t.put("000a.torrent", b"fine");
        let p = w.poll(&f).await;
        assert_eq!(p.added, 8, "the rest, in order");
        assert_eq!(t.top(), ["000a.torrent"]);
        assert_eq!(w.poll(&f).await.added, 1);
        assert!(t.top().is_empty());
    }

    #[tokio::test]
    async fn the_pipeline_closing_leaves_files_where_they_are() {
        let t = T::new();
        t.put("a.torrent", b"CLOSED");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        let p = settle(&mut w, &f).await;
        assert!(p.closed);
        assert_eq!(t.top(), ["a.torrent"]);
        assert!(t.names(ADDED).is_empty() && t.names(FAILED).is_empty());
    }

    #[test]
    fn a_folder_with_the_data_directory_is_never_watched() {
        let t = T::new();
        let inside = Watch {
            folder: t.data.path().join("x").to_string_lossy().into_owned(),
            after: After::Move,
        };
        std::fs::create_dir(t.data.path().join("x")).unwrap();
        std::fs::write(t.data.path().join("x").join("a.torrent"), b"fine").unwrap();
        let mut w = Watcher::new(&inside, t.data.path()).unwrap();
        let want = Some(Problem::Folder(FolderProblem::InsideDataDir));
        assert_eq!(w.check(), want);
        let f = Fake::default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for _ in 0..2 {
            assert_eq!(rt.block_on(w.poll(&f)).problem, want);
        }
        assert_eq!(calls(&f), 0, "nothing read");
        assert!(
            !t.data.path().join("x").join(ADDED).exists(),
            "nothing made"
        );
        let off = Watch::default();
        assert!(Watcher::new(&off, t.data.path()).is_none());
    }

    #[tokio::test]
    async fn other_entries_dont_hide_a_new_torrent() {
        let t = T::new();
        // More other entries than `.torrent` names a poll keeps.
        for i in 0..MAX_SCAN + 50 {
            t.put(&format!("{i:04}.txt"), b"x");
        }
        t.put("zz.torrent", b"fine");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        assert_eq!(settle(&mut w, &f).await.added, 1);
    }

    /// A change to the section that keeps the folder keeps what the
    /// watcher knows, so nothing it holds back is read again.
    #[tokio::test]
    async fn a_change_that_keeps_the_folder_keeps_its_memory() {
        let t = T::new();
        t.put("a.torrent", b"fine");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        w.poll(&f).await;
        let delete = Watch {
            folder: t.dir().to_string_lossy().into_owned(),
            after: After::Delete,
        };
        let mut shown = None;
        let mut w = apply(&delete, t.data.path(), Some(w), &mut shown).unwrap();
        // Seen once already: taken on this poll, and deleted now.
        assert_eq!(w.poll(&f).await.added, 1);
        assert!(t.top().is_empty() && t.names(ADDED).is_empty());
    }

    #[test]
    fn resume_order_wraps_after_the_cursor() {
        let names = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            resume_order(names(&["c", "a", "b"]), Some(OsStr::new("a"))),
            names(&["b", "c", "a"])
        );
        assert_eq!(resume_order(names(&["b", "a"]), None), names(&["a", "b"]));
        assert_eq!(
            resume_order(names(&["a", "b"]), Some(OsStr::new("z"))),
            names(&["a", "b"])
        );
    }

    #[test]
    fn torrent_names_and_their_targets() {
        assert!(is_torrent_name(OsStr::new("a.torrent")));
        assert!(is_torrent_name(OsStr::new("A.ToRrEnT")));
        assert!(!is_torrent_name(OsStr::new(".torrent")));
        assert!(!is_torrent_name(OsStr::new("a.torrents")));
        assert_eq!(target_name(OsStr::new("a.torrent"), None, 0), "a.torrent");
        assert_eq!(
            target_name(OsStr::new("a.TORRENT"), Some(Cause::Held), 2),
            "a.held-2.TORRENT"
        );
    }

    #[cfg(unix)]
    mod unix {
        use super::*;

        #[tokio::test]
        async fn a_link_is_failed_and_never_followed() {
            let t = T::new();
            let outside = tempfile::tempdir().unwrap();
            let secret = outside.path().join("secret.torrent");
            std::fs::write(&secret, b"fine").unwrap();
            std::os::unix::fs::symlink(&secret, t.dir().join("link.torrent")).unwrap();
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            let p = w.poll(&f).await;
            assert_eq!(p.failed, 1);
            assert_eq!(calls(&f), 0, "never read");
            assert_eq!(t.names(FAILED), ["link.link.torrent"]);
            // The link itself moved; its target is untouched.
            let moved = t.dir().join(FAILED).join("link.link.torrent");
            assert!(
                std::fs::symlink_metadata(&moved)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(std::fs::read(&secret).unwrap(), b"fine");
        }

        /// A file swapped for a link between the listing and the open is
        /// refused through the handle.
        #[test]
        fn a_swap_for_a_link_after_listing_is_refused() {
            let t = T::new();
            t.put("swap.torrent", b"fine");
            let folder = Folder::open(t.dir(), t.data.path()).ok().unwrap();
            let Some(Entry::File(shape)) = folder.entry(OsStr::new("swap.torrent")) else {
                panic!("listed as a file");
            };
            let outside = tempfile::tempdir().unwrap();
            let secret = outside.path().join("secret");
            std::fs::write(&secret, b"fine").unwrap();
            std::fs::remove_file(t.dir().join("swap.torrent")).unwrap();
            std::os::unix::fs::symlink(&secret, t.dir().join("swap.torrent")).unwrap();
            assert!(matches!(
                folder.read(OsStr::new("swap.torrent"), shape),
                Taken::Refuse(Cause::Link)
            ));
        }

        #[tokio::test]
        async fn a_hard_link_is_refused() {
            let t = T::new();
            let outside = tempfile::tempdir().unwrap();
            let other = outside.path().join("other");
            std::fs::write(&other, b"fine").unwrap();
            if std::fs::hard_link(&other, t.dir().join("hard.torrent")).is_err() {
                return; // Another filesystem: nothing to test.
            }
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            let p = settle(&mut w, &f).await;
            assert_eq!(p.failed, 1);
            assert_eq!(calls(&f), 0);
            assert_eq!(t.names(FAILED), ["hard.linked.torrent"]);
        }

        #[tokio::test]
        async fn added_as_a_link_stops_the_watch() {
            let t = T::new();
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), t.dir().join(ADDED)).unwrap();
            t.put("a.torrent", b"fine");
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            for _ in 0..2 {
                let p = w.poll(&f).await;
                assert_eq!(p.problem, Some(Problem::SubfolderLink));
            }
            assert_eq!(calls(&f), 0);
            assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
            // Fixed: it resumes on the next polls.
            std::fs::remove_file(t.dir().join(ADDED)).unwrap();
            let p = settle(&mut w, &f).await;
            assert_eq!((p.added, p.problem), (1, None));
            assert_eq!(t.names(ADDED), ["a.torrent"]);
        }

        #[tokio::test]
        async fn the_folder_turning_into_a_link_stops_the_watch() {
            let t = T::new();
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            w.poll(&f).await;
            let real = t.dir().with_extension("real");
            std::fs::rename(t.dir(), &real).unwrap();
            std::os::unix::fs::symlink(&real, t.dir()).unwrap();
            let p = w.poll(&f).await;
            assert_eq!(p.problem, Some(Problem::Folder(FolderProblem::Link)));
            std::fs::remove_file(t.dir()).unwrap();
            std::fs::rename(&real, t.dir()).unwrap();
        }

        /// A link that can't be moved is retried quietly: counted and
        /// reported once, not on every poll.
        #[tokio::test]
        async fn a_link_that_cant_be_moved_is_reported_once() {
            let t = T::new();
            let outside = tempfile::tempdir().unwrap();
            let name = format!("{}.torrent", "l".repeat(244));
            std::os::unix::fs::symlink(outside.path(), t.dir().join(&name)).unwrap();
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            assert_eq!(w.poll(&f).await.failed, 1);
            for _ in 0..3 {
                assert_eq!(w.poll(&f).await, Polled::default());
            }
            assert_eq!(w.stuck.len(), 1);
        }

        /// A folder written with a trailing separator is still checked as
        /// itself: a link written `link/` is refused, and a folder swapped
        /// for one into the data directory is never read.
        #[tokio::test]
        async fn a_trailing_separator_doesnt_hide_a_link() {
            let t = T::new();
            let link = t.root.path().join("link");
            std::os::unix::fs::symlink(t.data.path(), &link).unwrap();
            std::fs::write(t.data.path().join("in.torrent"), b"fine").unwrap();
            let w = Watch {
                folder: format!("{}/", link.display()),
                after: After::Move,
            };
            assert!(crate::settings::Section::check(&w, "folder").is_err());
            let mut watcher = Watcher::new(&w, t.data.path()).unwrap();
            let f = Fake::default();
            for _ in 0..2 {
                assert_eq!(
                    watcher.poll(&f).await.problem,
                    Some(Problem::Folder(FolderProblem::Link))
                );
            }
            assert_eq!(calls(&f), 0);
            assert_eq!(
                std::fs::read(t.data.path().join("in.torrent")).unwrap(),
                b"fine"
            );
        }

        /// A file that can't be moved (here, its name in `failed/` would
        /// be too long, which binds even for root) is remembered, never
        /// read again, reported once each, and a full set stops the watch.
        #[tokio::test]
        async fn files_that_cant_be_moved_are_never_read_again() {
            crate::test_log::install();
            let t = T::new();
            let long = |i: usize| format!("{}{i}.torrent", "q".repeat(240));
            t.put(&long(0), b"MALF");
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            let p = settle(&mut w, &f).await;
            assert_eq!((p.failed, p.problem), (1, None));
            for _ in 0..3 {
                assert_eq!(w.poll(&f).await.failed, 0);
            }
            assert_eq!(calls(&f), 1, "never read again");
            // Fill the set: the watch stops.
            for i in 1..MAX_STUCK {
                t.put(&long(i), b"MALF");
            }
            w.poll(&f).await;
            let p = w.poll(&f).await;
            assert_eq!(p.problem, Some(Problem::TooManyStuck));
            // It holds: nothing more is read.
            let before = calls(&f);
            assert_eq!(w.poll(&f).await.problem, Some(Problem::TooManyStuck));
            assert_eq!(calls(&f), before);
            let log = crate::test_log::text();
            assert!(!log.contains("qqqqqqqq"), "no file name: {log}");
            assert!(log.contains("event=\"watch.stuck\""), "{log}");
        }
    }
}
