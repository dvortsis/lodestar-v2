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
//! - **A Malformed verdict is held for [`MALFORMED_SPAN`]**
//!   (X-watch-stall-retry): a copy over SMB or NFS that stalls (a disk
//!   spinning up, Wi-Fi roaming) looks complete and reads as malformed.
//!   Such a file stays where it is, and isn't read again until it has
//!   stayed unchanged for 60 s since that read. It goes to `failed/` only
//!   if it reads as malformed then, with the same size and modification
//!   time. One that changed is still being written: the span starts again
//!   from its next read. A file is pending at most [`PENDING_FOR`] from
//!   its first malformed read, however often it changes, then failed; at
//!   most [`MAX_HELD`] files are pending at once, and past that the
//!   oldest is failed.
//! - **Never read twice,** but for a held Malformed verdict (above), the
//!   one deliberate re-read. A file that can't be moved or deleted yet is
//!   remembered in a bounded set, never read again, and its move retried
//!   quietly each poll; when the set is full the watch holds. A name that
//!   leaves the folder is forgotten once a whole listing shows it gone.
//! - **On Windows a link entry stays where it is,** never read: it's
//!   remembered in a bounded set of its own, which never holds the watch
//!   (rv-765 I3). The folder, `added\` and `failed\` are held open for
//!   the whole poll without delete sharing, so none of them can be renamed
//!   or swapped for a junction while it runs (rv-765 I2).
//! - **No file name in any log line or status**, only counts and causes:
//!   names in a user's folder are often torrent titles.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

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
/// How long a file read as malformed must stay unchanged before a second
/// malformed read fails it: 60 s of wall-clock time, whatever the poll
/// interval (runner, 2026-11-13, a §2.26 number). A single stall under
/// about 70 s always survives; one over about 90 s doesn't.
pub const MALFORMED_SPAN: Duration = Duration::from_secs(60);
/// How long a file may stay pending after its first malformed read, at
/// most, however often it changes: 10 min of wall-clock time (runner,
/// 2026-11-13, a §2.26 number). Then it goes to `failed/` as malformed,
/// without another read. A copy that's slow but still making progress
/// has that long to finish.
pub const PENDING_FOR: Duration = Duration::from_secs(600);
/// Files pending after a malformed read, at most (a §2.26 number, as
/// [`MAX_STUCK`]). A new one past that sends the oldest pending file to
/// `failed/` as malformed, counted in `watch.took`'s `pending_full`.
/// Tests use 4.
pub const MAX_HELD: usize = if cfg!(test) { 4 } else { 1024 };
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
    /// how many to `failed/`, of those how many because judging failed on
    /// Lodestar's side (`errors`), not because of the file, and how many
    /// because too many files were pending after a malformed read
    /// (`pending_full`: the oldest pending file was failed to make room).
    WATCH_TOOK {
        id: "watch.took",
        component: Sources,
        level: Info,
        message: "Took .torrent files from the watch folder",
        fields: { added: Label, failed: Label, errors: Label, pending_full: Label },
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
    /// Files seen but not put away yet: still changing, held open, past
    /// this poll's [`MAX_SUBMIT`], or read as malformed and held.
    pub waiting: u64,
    /// Of `waiting`, files pending after a malformed read: kept for a
    /// later read rather than failed (not the filter's `held` verdict).
    pub rechecking: u64,
    /// Of `failed`, pending files failed early because [`MAX_HELD`] were
    /// pending and a new one came: the oldest makes room.
    pub pending_full: u64,
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

/// A file read as malformed and not failed yet: read again once it has
/// stayed unchanged for [`MALFORMED_SPAN`], and failed at the latest
/// [`PENDING_FOR`] after `first`.
#[derive(Debug, Clone, Copy)]
struct Held {
    /// Its shape when it was last read as malformed.
    shape: Shape,
    /// When it was first read as malformed with this shape.
    since: Instant,
    /// When it was first read as malformed at all.
    first: Instant,
}

/// What [`Watcher::hold`] decided for a file just read as malformed.
#[derive(Debug, PartialEq, Eq)]
enum Hold {
    /// Fail it now.
    Fail,
    /// Keep it pending.
    Keep,
    /// Keep it pending, and fail the oldest pending file, named, to make
    /// room.
    Evict(OsString, Shape),
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
    /// Link entries left where they are (Windows: a junction, a OneDrive
    /// placeholder), by name: never read, counted once, and never
    /// holding the watch. At most [`MAX_STUCK`]; past that, one more is
    /// counted again each poll, nothing worse.
    left: HashSet<OsString>,
    /// Files read as malformed and held for one more read, by name. At
    /// most [`MAX_HELD`]; in memory only, so a restart judges them afresh.
    held: HashMap<OsString, Held>,
    /// How far tests have moved [`Watcher::now`] forward.
    #[cfg(test)]
    skew: Duration,
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
            left: HashSet::new(),
            held: HashMap::new(),
            #[cfg(test)]
            skew: Duration::ZERO,
        })
    }

    /// Why the folder can't be watched now, if it can't. (The watch
    /// itself checks on the blocking pool, in [`apply`] and [`Folder::open`].)
    #[cfg(test)]
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
            self.left = before.left;
            self.held = before.held;
            #[cfg(test)]
            {
                self.skew = before.skew;
            }
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
                let (names, whole) = f.list().map_err(|_| Problem::Unreadable)?;
                // Every name listed, so one whose entry couldn't be read
                // this time is never forgotten (rv-765 P1).
                let listed = whole.then(|| names.iter().cloned().collect::<HashSet<_>>());
                let entries: Vec<_> = resume_order(names, cursor.as_deref())
                    .into_iter()
                    .filter_map(|n| f.entry(&n).map(|e| (n, e)))
                    .collect();
                Ok((entries, listed, f))
            })
        })
        .await;
        let (entries, listed, folder) = match opened {
            Ok((entries, listed, f)) => (entries, listed, std::sync::Arc::new(f)),
            Err(p) => {
                out.problem = Some(p);
                return out;
            }
        };
        // rv-765 O1: a whole listing shows which remembered names are gone.
        if let Some(listed) = listed {
            self.stuck.retain(|n, _| listed.contains(n));
            self.left.retain(|n| listed.contains(n));
            self.held.retain(|n, _| listed.contains(n));
        }
        let mut seen = HashMap::new();
        let mut submitted = 0;
        // Names an eviction dealt with this poll: what's there now is
        // judged by a later poll, never by this one's stale listing.
        let mut evicted: HashSet<OsString> = HashSet::new();
        let mut entries = entries.into_iter();
        for (name, entry) in entries.by_ref() {
            // A problem putting files away (rv-765 B2), or the set full:
            // nothing more is taken this poll.
            if self.halted || out.problem.is_some() {
                seen.extend(self.seen.remove_entry(&name));
                break;
            }
            if evicted.contains(&name) {
                continue;
            }
            if self.retry_stuck(&folder, &name, &entry, &mut out).await {
                continue;
            }
            match entry {
                Entry::Link if self.left.contains(&name) => continue,
                Entry::Link => {}
                _ => {
                    self.left.remove(&name);
                }
            }
            let shape = match entry {
                Entry::Other => {
                    // A held name that's now a folder or other entry is no
                    // longer pending: it keeps no slot.
                    self.held.remove(&name);
                    continue;
                }
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
            if self
                .held
                .get(&name)
                .is_some_and(|h| self.now().duration_since(h.first) >= PENDING_FOR)
            {
                // Pending too long, however it changed: failed unread.
                self.finish(
                    &folder,
                    &name,
                    Some(shape),
                    Some(Cause::Malformed),
                    &mut out,
                )
                .await;
                continue;
            }
            if self.waits_unread(&name, shape) {
                out.waiting += 1;
                out.rechecking += 1;
                continue;
            }
            if self.seen.get(&name) != Some(&shape) || submitted == MAX_SUBMIT {
                out.waiting += 1;
                if self.held.contains_key(&name) {
                    out.rechecking += 1;
                }
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
            if cause == Some(Cause::Malformed) {
                match self.hold(&name, shape) {
                    Hold::Fail => {}
                    Hold::Keep => {
                        out.waiting += 1;
                        out.rechecking += 1;
                        continue;
                    }
                    Hold::Evict(oldest, its) => {
                        out.waiting += 1;
                        out.rechecking += 1;
                        self.evict(&folder, &oldest, its, &mut out).await;
                        evicted.insert(oldest);
                        continue;
                    }
                }
            }
            self.finish(&folder, &name, Some(shape), cause, &mut out)
                .await;
        }
        // What a stop left unvisited keeps its last shape, so it's not
        // made to wait a poll more.
        for (name, _) in entries {
            seen.extend(self.seen.remove_entry(&name));
        }
        self.seen = seen;
        if self.halted {
            out.problem = Some(Problem::TooManyStuck);
        }
        out
    }

    /// The time spans are measured on. Tests move it forward with `skew`,
    /// so a 60 s span or 10 min limit passes without waiting for it.
    fn now(&self) -> Instant {
        #[cfg(test)]
        return Instant::now() + self.skew;
        #[cfg(not(test))]
        Instant::now()
    }

    /// Whether a held file is left unread this poll: unchanged since its
    /// malformed read, and [`MALFORMED_SPAN`] not over yet.
    fn waits_unread(&self, name: &OsStr, shape: Shape) -> bool {
        self.held.get(name).is_some_and(|h| {
            h.shape == shape && self.now().duration_since(h.since) < MALFORMED_SPAN
        })
    }

    /// Whether a file just read as malformed is kept pending or failed.
    /// Failed when an earlier read found it malformed with this same
    /// shape [`MALFORMED_SPAN`] or more ago (it isn't changing, so it's
    /// really malformed), or when it has been pending [`PENDING_FOR`]. A
    /// new shape starts a new span, not a new pending time. A new pending
    /// file past [`MAX_HELD`] makes room by failing the oldest.
    fn hold(&mut self, name: &OsStr, shape: Shape) -> Hold {
        let now = self.now();
        let (first, evict) = match self.held.get(name) {
            Some(h) if now.duration_since(h.first) >= PENDING_FOR => return Hold::Fail,
            // Reached only through a race: an unchanged file isn't read
            // while its span runs (`waits_unread`).
            Some(h) if h.shape == shape => {
                return if now.duration_since(h.since) < MALFORMED_SPAN {
                    Hold::Keep
                } else {
                    Hold::Fail
                };
            }
            Some(h) => (h.first, None),
            None if self.held.len() >= MAX_HELD => {
                let oldest = self
                    .held
                    .iter()
                    .min_by_key(|(_, h)| h.first)
                    .map(|(n, h)| (n.clone(), h.shape));
                (now, oldest)
            }
            None => (now, None),
        };
        if let Some((oldest, _)) = &evict {
            self.held.remove(oldest);
        }
        self.held.insert(
            name.to_os_string(),
            Held {
                shape,
                since: now,
                first,
            },
        );
        match evict {
            Some((oldest, its)) => Hold::Evict(oldest, its),
            None => Hold::Keep,
        }
    }

    /// For a file taken before but not moved: retries its move quietly
    /// (no count, no log) and says it's handled. A file that changed since
    /// is a new file: forgotten here, and judged afresh. One still over
    /// the cap stays stuck under its name, however it grows. A problem
    /// with `added/` or `failed/` stops the poll (rv-765 P3).
    async fn retry_stuck(
        &mut self,
        folder: &std::sync::Arc<Folder>,
        name: &OsStr,
        entry: &Entry,
        out: &mut Polled,
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
        match self.put_away(folder, name, stuck.cause).await {
            Ok(()) => {
                self.stuck.remove(name);
            }
            Err(MoveError::Subfolder(p)) => out.problem = Some(p),
            Err(MoveError::Gone) => {
                self.stuck.remove(name);
            }
            Err(_) => {}
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
        self.held.remove(name);
        match cause {
            None => out.added += 1,
            Some(Cause::Failed) => {
                out.failed += 1;
                out.errors += 1;
            }
            Some(_) => out.failed += 1,
        }
        let moved = self.put_away(folder, name, cause).await;
        self.after_move(name, shape, cause, moved, out);
    }

    /// Fails the oldest pending file to make room in a full held set,
    /// counted in `failed` and `pending_full`, if a regular file is still
    /// there by that name. Anything else (gone, a folder, a link) is only
    /// dropped from the set; a later poll judges what's there as usual.
    async fn evict(
        &mut self,
        folder: &std::sync::Arc<Folder>,
        name: &OsStr,
        shape: Shape,
        out: &mut Polled,
    ) {
        let (f, n) = (folder.clone(), name.to_os_string());
        if !matches!(blocking(move || f.entry(&n)).await, Some(Entry::File(_))) {
            return;
        }
        let cause = Some(Cause::Malformed);
        let moved = self.put_away(folder, name, cause).await;
        if !matches!(moved, Err(MoveError::Gone)) {
            out.failed += 1;
            out.pending_full += 1;
        }
        self.after_move(name, Some(shape), cause, moved, out);
    }

    /// What a move's result leaves to remember: a file that couldn't be
    /// moved is never read again, and reported once; when [`MAX_STUCK`]
    /// are, the watch holds. One that's gone leaves nothing.
    fn after_move(
        &mut self,
        name: &OsStr,
        shape: Option<Shape>,
        cause: Option<Cause>,
        moved: Result<(), MoveError>,
        out: &mut Polled,
    ) {
        match moved {
            Ok(()) | Err(MoveError::Gone) => {}
            // Checked again next poll; the file stays where it is, and the
            // poll stops (rv-765 B2).
            Err(MoveError::Subfolder(p)) => {
                self.remember(name, Stuck { shape, cause });
                out.problem = Some(p);
            }
            Err(MoveError::Io) => {
                if self.remember(name, Stuck { shape, cause }) {
                    log_event!(WATCH_STUCK, stuck = Count(self.stuck.len() as u64));
                }
            }
            Err(MoveError::Left) => {
                if self.left.len() < MAX_STUCK {
                    self.left.insert(name.to_os_string());
                }
            }
        }
    }

    /// Adds `name` to the never-read-again set, which never passes
    /// [`MAX_STUCK`]: once full, the watch holds. Whether it was added.
    fn remember(&mut self, name: &OsStr, stuck: Stuck) -> bool {
        if self.stuck.len() >= MAX_STUCK {
            self.halted = true;
            return false;
        }
        self.stuck.insert(name.to_os_string(), stuck);
        if self.stuck.len() >= MAX_STUCK {
            self.halted = true;
        }
        true
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
    /// Nothing is there by that name any more (removed by hand, say):
    /// nothing to move, and nothing to remember.
    Gone,
    /// A link entry that isn't moved (Windows): left where it is.
    #[cfg_attr(not(windows), allow(dead_code))]
    Left,
}

/// The folder for one poll, opened without following links, with its
/// `added/` and `failed/` checked.
struct Folder {
    path: PathBuf,
    #[cfg(unix)]
    fd: rustix::fd::OwnedFd,
    /// The folder, `added\` and `failed\`, held open for the poll without
    /// delete sharing: none can be renamed, deleted or replaced by a
    /// junction until it ends (rv-765 I2). Unix uses `fd` instead.
    #[cfg(windows)]
    _held: Vec<std::fs::File>,
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
        #[cfg(windows)]
        let held = hold(
            path,
            Problem::Folder(FolderProblem::Link),
            Problem::Folder(FolderProblem::NotDirectory),
            Problem::Unreadable,
        )?;
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut f = Folder {
            path: path.to_path_buf(),
            #[cfg(unix)]
            fd,
            #[cfg(windows)]
            _held: vec![held],
        };
        for sub in [ADDED, FAILED] {
            f.subfolder(sub)?;
            #[cfg(windows)]
            f._held.push(hold(
                &path.join(sub),
                Problem::SubfolderLink,
                Problem::SubfolderNotDirectory,
                Problem::SubfolderCreate,
            )?);
        }
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
    /// there can't hide a new torrent. With whether the listing is whole:
    /// read to its end, with no entry it couldn't read.
    fn list(&self) -> std::io::Result<(Vec<OsString>, bool)> {
        let mut names = Vec::new();
        let mut whole = true;
        for (read, entry) in std::fs::read_dir(&self.path)?.enumerate() {
            if read == MAX_LIST || names.len() == MAX_SCAN {
                whole = false;
                break;
            }
            let Ok(entry) = entry else {
                whole = false;
                continue;
            };
            let name = entry.file_name();
            if is_torrent_name(&name) {
                names.push(name);
            }
        }
        Ok((names, whole))
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
        // A link entry stays where it is on Windows (see
        // `rename_no_replace`), told by its own entry and nothing else, so
        // no other refusal (a share without hard links) leaves a file
        // that would be read again (rv-765 P2).
        #[cfg(windows)]
        if std::fs::symlink_metadata(self.path.join(name)).is_ok_and(|m| is_link(&m)) {
            return Err(MoveError::Left);
        }
        for n in 0..MAX_SUFFIX {
            let target = target_name(name, cause, n);
            match self.rename_no_replace(name, sub, &target) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                // Gone only if the file itself is: a missing `added/` or
                // `failed/` leaves it stuck, never read again.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.absent(name) => {
                    return Err(MoveError::Gone);
                }
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
    ///
    /// A file that couldn't be removed (held open without delete sharing,
    /// read-only) is found before the link is made, and a removal that
    /// still fails takes the new link back, so a retried move never
    /// leaves names behind in `added\` or `failed\` (rv-765 I1).
    #[cfg(windows)]
    fn rename_no_replace(&self, name: &OsStr, sub: &str, target: &OsStr) -> std::io::Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        const DELETE: u32 = 0x0001_0000;
        const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let from = self.path.join(name);
        if std::fs::symlink_metadata(&from).is_ok_and(|m| is_link(&m)) {
            return Err(std::io::ErrorKind::Unsupported.into());
        }
        // Whether it could be removed now, before any link is made.
        drop(
            std::fs::OpenOptions::new()
                .access_mode(DELETE)
                .share_mode(FILE_SHARE_ALL)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&from)?,
        );
        let to = self.path.join(sub).join(target);
        std::fs::hard_link(&from, &to)?;
        std::fs::remove_file(&from).inspect_err(|_| {
            let _ = std::fs::remove_file(&to);
        })
    }

    fn remove(&self, name: &OsStr) -> Result<(), MoveError> {
        #[cfg(unix)]
        let done = rustix::fs::unlinkat(&self.fd, name, rustix::fs::AtFlags::empty())
            .map_err(std::io::Error::from);
        #[cfg(windows)]
        let done = std::fs::remove_file(self.path.join(name));
        done.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound if self.absent(name) => MoveError::Gone,
            _ => MoveError::Io,
        })
    }

    /// Whether nothing is in the folder by `name`, read without following
    /// it (through the folder's handle on Unix).
    fn absent(&self, name: &OsStr) -> bool {
        #[cfg(unix)]
        let found = rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map(drop)
            .map_err(std::io::Error::from);
        #[cfg(windows)]
        let found = std::fs::symlink_metadata(self.path.join(name)).map(drop);
        found.is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    }
}

/// Opens the directory at `path` itself, never what a junction there
/// points to, without delete sharing, and checks through the handle that
/// it's a directory and not a reparse point: `link`, `not_dir`, or
/// `failed` when it can't be opened.
#[cfg(windows)]
fn hold(
    path: &Path,
    link: Problem,
    not_dir: Problem,
    failed: Problem,
) -> Result<std::fs::File, Problem> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    // FILE_SHARE_READ | FILE_SHARE_WRITE, never FILE_SHARE_DELETE.
    const SHARE: u32 = 0x1 | 0x2;
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| failed)?;
    let meta = dir.metadata().map_err(|_| failed)?;
    if is_link(&meta) {
        Err(link)
    } else if !meta.is_dir() {
        Err(not_dir)
    } else {
        Ok(dir)
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
        let mut watcher = apply(&first, &data_dir, None, &mut shown).await;
        let mut tick = tokio::time::interval(POLL_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let now = rx.borrow_and_update().clone();
                    watcher = apply(&now, &data_dir, watcher.take(), &mut shown).await;
                }
                _ = tick.tick() => {
                    let Some(w) = watcher.as_mut() else { continue };
                    let polled = w.poll(&uploads).await;
                    if polled.added + polled.failed > 0 {
                        log_event!(
                            WATCH_TOOK,
                            added = Count(polled.added),
                            failed = Count(polled.failed),
                            errors = Count(polled.errors),
                            pending_full = Count(polled.pending_full)
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
/// folder, with the change logged. The folder is looked at on the
/// blocking pool: a hung network mount never holds a runtime thread
/// (rv-765 O3).
async fn apply(
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
    let (folder, data) = (w.folder.clone(), w.data_dir.clone());
    let problem = blocking(move || folder_problem(&folder, &data).map(Problem::Folder)).await;
    show(problem, shown);
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
    async fn settle<S: Sink>(w: &mut Watcher, f: &S) -> Polled {
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
        assert_eq!((p.added, p.failed, p.errors, p.rechecking), (1, 3, 1, 1));
        // Malformed is held; unchanged for MALFORMED_SPAN, it's failed.
        assert_eq!(t.top(), ["a.torrent"]);
        age(&mut w);
        let p = w.poll(&f).await;
        assert_eq!((p.added, p.failed, p.rechecking), (0, 1, 0));
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

    /// Appends to a file in the folder, as a copy that resumes does.
    fn append(t: &T, name: &str, more: &[u8]) {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(t.dir().join(name))
            .unwrap();
        std::io::Write::write_all(&mut file, more).unwrap();
    }

    /// As if [`MALFORMED_SPAN`] of wall-clock time had passed: the span
    /// stays 60 s, and the tests control the clock it's measured on.
    fn age(w: &mut Watcher) {
        age_by(w, MALFORMED_SPAN);
    }

    /// Moves the watcher's clock forward by `by`. Moving the clock, not
    /// the instants back, can't underflow on a machine just booted.
    fn age_by(w: &mut Watcher, by: Duration) {
        w.skew += by;
    }

    /// X-watch-stall-retry: a really malformed file that doesn't change is
    /// left unread while MALFORMED_SPAN runs, read once more after it,
    /// failed, and never read again.
    #[tokio::test]
    async fn an_unchanged_malformed_file_is_failed_after_the_span() {
        let t = T::new();
        t.put("bad.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        let p = settle(&mut w, &f).await;
        assert_eq!((p.failed, p.rechecking, p.waiting), (0, 1, 1));
        assert_eq!(t.top(), ["bad.torrent"]);
        // Within the span: not read, not failed, however many polls.
        for _ in 0..3 {
            let p = w.poll(&f).await;
            assert_eq!((p.failed, p.rechecking), (0, 1));
        }
        assert_eq!(calls(&f), 1);
        age(&mut w);
        let p = w.poll(&f).await;
        assert_eq!((p.failed, p.rechecking), (1, 0));
        assert_eq!(t.names(FAILED), ["bad.malformed.torrent"]);
        assert!(w.held.is_empty());
        w.poll(&f).await;
        assert_eq!(calls(&f), 2);
    }

    /// X-watch-stall-retry: a change resets the span, so a file that
    /// changed is never failed on the strength of an older read.
    #[tokio::test]
    async fn a_change_starts_the_span_again() {
        let t = T::new();
        t.put("slow.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        settle(&mut w, &f).await;
        let first = w.held[OsStr::new("slow.torrent")].since;
        age(&mut w);
        append(&t, "slow.torrent", b"+");
        // Changed: waits to be still, read, malformed with a new shape,
        // held again with a fresh span rather than failed.
        let p = settle(&mut w, &f).await;
        assert_eq!((p.failed, p.rechecking), (0, 1));
        assert!(w.held[OsStr::new("slow.torrent")].since > first);
        assert_eq!(w.poll(&f).await.failed, 0);
    }

    /// X-watch-stall-retry: a copy that stalls looks complete and reads as
    /// malformed; once it resumes and finishes, it's judged afresh and
    /// taken, never failed.
    #[tokio::test]
    async fn a_stalled_copy_that_resumes_is_taken() {
        let t = T::new();
        t.put("slow.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        assert_eq!(settle(&mut w, &f).await.rechecking, 1);
        // The copy resumes and finishes (a new size): it waits a poll to
        // be still, as any changing file does, and isn't read meanwhile.
        t.put("slow.torrent", b"d4:infodee");
        let p = w.poll(&f).await;
        assert_eq!((p.added, p.failed, p.waiting), (0, 0, 1), "changed: waits");
        assert_eq!(calls(&f), 1);
        // Still since: judged afresh, and taken.
        let p = w.poll(&f).await;
        assert_eq!((p.added, p.failed), (1, 0));
        assert_eq!(t.names(ADDED), ["slow.torrent"]);
        assert!(t.names(FAILED).is_empty() && w.held.is_empty());
    }

    /// X-watch-stall-retry: a slow copy that changes between each
    /// malformed read, more often than any small count would allow, stays
    /// pending and is taken once it's complete.
    #[tokio::test]
    async fn a_file_that_keeps_changing_is_taken_once_complete() {
        let t = T::new();
        t.put("slow.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        for round in 0..8 {
            let p = settle(&mut w, &f).await;
            assert_eq!((p.failed, p.rechecking), (0, 1), "round {round}");
            append(&t, "slow.torrent", b"+");
        }
        t.put("slow.torrent", b"d4:infodee");
        let p = settle(&mut w, &f).await;
        assert_eq!((p.added, p.failed), (1, 0));
        assert_eq!(t.names(ADDED), ["slow.torrent"]);
        assert!(w.held.is_empty());
    }

    /// X-watch-stall-retry: PENDING_FOR after its first malformed read, a
    /// file goes to `failed/` however it changed since, without another
    /// read, so one that keeps changing isn't pending for ever.
    #[tokio::test]
    async fn a_file_pending_past_the_limit_is_failed() {
        let t = T::new();
        t.put("grow.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        settle(&mut w, &f).await;
        // Changed since, and a new span started: the pending time doesn't.
        append(&t, "grow.torrent", b"+");
        settle(&mut w, &f).await;
        assert_eq!(calls(&f), 2);
        age_by(&mut w, PENDING_FOR - MALFORMED_SPAN);
        append(&t, "grow.torrent", b"+");
        assert_eq!(w.poll(&f).await.failed, 0, "just within the limit");
        age_by(&mut w, MALFORMED_SPAN);
        append(&t, "grow.torrent", b"+");
        let p = w.poll(&f).await;
        assert_eq!((p.failed, p.rechecking), (1, 0));
        assert_eq!(t.names(FAILED), ["grow.malformed.torrent"]);
        assert_eq!(calls(&f), 2, "failed unread");
        assert!(w.held.is_empty());
    }

    /// X-watch-stall-retry: at most MAX_HELD files are pending; a new one
    /// past that fails the oldest, counted apart, and only Malformed is
    /// ever held.
    #[tokio::test]
    async fn the_held_set_is_bounded_by_failing_the_oldest() {
        let t = T::new();
        t.put("m0.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        settle(&mut w, &f).await;
        age_by(&mut w, Duration::from_secs(1));
        for i in 1..=MAX_HELD {
            t.put(&format!("m{i}.torrent"), b"MALF");
        }
        t.put("z.torrent", b"FLOOR");
        let p = settle(&mut w, &f).await;
        assert_eq!(
            (p.rechecking, p.failed, p.pending_full),
            (MAX_HELD as u64, 2, 1)
        );
        assert_eq!(w.held.len(), MAX_HELD);
        assert!(!w.held.contains_key(OsStr::new("m0.torrent")));
        assert_eq!(
            t.names(FAILED),
            ["m0.malformed.torrent", "z.refused.torrent"]
        );
        age(&mut w);
        let p = w.poll(&f).await;
        assert_eq!(
            (p.rechecking, p.failed, p.pending_full),
            (0, MAX_HELD as u64, 0)
        );
        assert!(w.held.is_empty());
    }

    /// Evicting a pending file that someone removed meanwhile leaves no
    /// stuck entry and counts nothing: there was nothing to move.
    #[tokio::test]
    async fn evicting_a_file_thats_gone_leaves_nothing_behind() {
        /// Removes `gone` on the first new file it's sent.
        struct Remover<'a> {
            inner: Fake,
            gone: &'a Path,
            done: std::sync::atomic::AtomicBool,
        }
        impl Sink for Remover<'_> {
            async fn submit(&self, body: Vec<u8>) -> Result<Outcome, Closed> {
                if body.ends_with(b"NEW") && !self.done.swap(true, Ordering::SeqCst) {
                    std::fs::remove_file(self.gone).unwrap();
                }
                self.inner.submit(body).await
            }
        }
        let t = T::new();
        t.put("m0.torrent", b"MALF");
        let gone = t.dir().join("m0.torrent");
        let s = Remover {
            inner: Fake::default(),
            gone: &gone,
            done: Default::default(),
        };
        let mut w = t.watcher(After::Move);
        settle(&mut w, &s).await;
        age_by(&mut w, Duration::from_secs(1));
        for i in 1..=MAX_HELD {
            t.put(&format!("m{i}.torrent"), b"MALF NEW");
        }
        let p = settle(&mut w, &s).await;
        assert_eq!((p.failed, p.pending_full), (0, 0));
        assert!(w.stuck.is_empty(), "nothing remembered for a gone file");
        assert_eq!(w.held.len(), MAX_HELD);
        assert!(!w.held.contains_key(OsStr::new("m0.torrent")));
        assert!(t.names(FAILED).is_empty());
    }

    /// A held name replaced by a folder is never moved by an eviction,
    /// and a held name that turns into something other than a file keeps
    /// no slot.
    #[tokio::test]
    async fn eviction_moves_only_a_regular_file() {
        /// Swaps `m0.torrent` for a folder on the first new file it's sent.
        struct Swapper<'a> {
            inner: Fake,
            at: &'a Path,
            done: std::sync::atomic::AtomicBool,
        }
        impl Sink for Swapper<'_> {
            async fn submit(&self, body: Vec<u8>) -> Result<Outcome, Closed> {
                if body.ends_with(b"NEW") && !self.done.swap(true, Ordering::SeqCst) {
                    std::fs::remove_file(self.at).unwrap();
                    std::fs::create_dir(self.at).unwrap();
                    std::fs::write(self.at.join("inside"), b"x").unwrap();
                }
                self.inner.submit(body).await
            }
        }
        let t = T::new();
        t.put("m0.torrent", b"MALF");
        let at = t.dir().join("m0.torrent");
        let s = Swapper {
            inner: Fake::default(),
            at: &at,
            done: Default::default(),
        };
        let mut w = t.watcher(After::Move);
        settle(&mut w, &s).await;
        age_by(&mut w, Duration::from_secs(1));
        for i in 1..=MAX_HELD {
            t.put(&format!("m{i}.torrent"), b"MALF NEW");
        }
        let p = settle(&mut w, &s).await;
        assert_eq!((p.failed, p.pending_full), (0, 0));
        // Not looked at again this poll from the stale listing: on Windows
        // that read fails as `unreadable` (rv: CI on #837).
        assert_eq!(p.waiting, MAX_HELD as u64);
        assert!(at.join("inside").exists(), "the folder stays where it is");
        assert!(t.names(FAILED).is_empty());
        assert!(w.stuck.is_empty() && !w.held.contains_key(OsStr::new("m0.torrent")));
        // A held name that becomes a folder between polls is dropped too.
        std::fs::remove_file(t.dir().join("m1.torrent")).unwrap();
        std::fs::create_dir(t.dir().join("m1.torrent")).unwrap();
        w.poll(&s.inner).await;
        assert!(!w.held.contains_key(OsStr::new("m1.torrent")));
    }

    /// A rename that fails with "not found" because `failed/` went
    /// missing mid-move is no proof the file is gone: `absent` says it's
    /// still there, so the move is `Io` (stuck, never read again), not
    /// `Gone`.
    #[cfg(unix)]
    #[test]
    fn a_missing_subfolder_mid_move_is_not_gone() {
        let t = T::new();
        t.put("a.torrent", b"FLOOR");
        let folder = Folder::open(t.dir(), t.data.path()).unwrap();
        std::fs::remove_dir(t.dir().join(FAILED)).unwrap();
        let e = folder
            .rename_no_replace(
                OsStr::new("a.torrent"),
                FAILED,
                OsStr::new("a.refused.torrent"),
            )
            .unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        assert!(!folder.absent(OsStr::new("a.torrent")));
        assert!(folder.absent(OsStr::new("gone.torrent")));
    }

    /// An evicted file that sorts after the one that pushed it out is
    /// skipped for the rest of the poll: neither read again, nor counted
    /// again, nor failed twice.
    #[tokio::test]
    async fn an_evicted_file_later_in_the_poll_is_not_read() {
        let t = T::new();
        t.put("z0.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        settle(&mut w, &f).await;
        age_by(&mut w, Duration::from_secs(1));
        for i in 1..=MAX_HELD {
            t.put(&format!("a{i}.torrent"), b"MALF");
        }
        let p = settle(&mut w, &f).await;
        assert_eq!(
            (p.failed, p.pending_full, p.waiting),
            (1, 1, MAX_HELD as u64)
        );
        assert_eq!(t.names(FAILED), ["z0.malformed.torrent"]);
        assert_eq!(calls(&f), 1 + MAX_HELD, "z0 read once only");
        assert!(w.stuck.is_empty());
    }

    /// A held file removed by hand is forgotten once a whole listing shows
    /// it gone.
    #[tokio::test]
    async fn a_held_file_removed_by_hand_is_forgotten() {
        let t = T::new();
        t.put("gone.torrent", b"MALF");
        let f = Fake::default();
        let mut w = t.watcher(After::Move);
        settle(&mut w, &f).await;
        assert_eq!(w.held.len(), 1);
        std::fs::remove_file(t.dir().join("gone.torrent")).unwrap();
        w.poll(&f).await;
        assert!(w.held.is_empty());
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
        assert_eq!((p.added, p.failed, p.rechecking), (1, 0, 1));
        age(&mut w);
        assert_eq!(w.poll(&f).await.failed, 1);
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
        let mut w = apply(&delete, t.data.path(), Some(w), &mut shown)
            .await
            .unwrap();
        // Seen once already: taken on this poll, and deleted now.
        assert_eq!(w.poll(&f).await.added, 1);
        assert!(t.top().is_empty() && t.names(ADDED).is_empty());
        // A held Malformed verdict survives a change too: the next read
        // fails the file, with no fresh hold.
        t.put("b.torrent", b"MALF");
        assert_eq!(settle(&mut w, &f).await.rechecking, 1);
        let move_ = Watch {
            folder: t.dir().to_string_lossy().into_owned(),
            after: After::Move,
        };
        let mut w = apply(&move_, t.data.path(), Some(w), &mut shown)
            .await
            .unwrap();
        assert_eq!(w.poll(&f).await.failed, 0, "the span carries over");
        age(&mut w);
        assert_eq!(w.poll(&f).await.failed, 1);
        assert_eq!(t.names(FAILED), ["b.malformed.torrent"]);
        assert_eq!(calls(&f), 3);
    }

    /// rv-765 O5: at most [`MAX_SCAN`] `.torrent` names a poll, and such a
    /// listing isn't whole.
    #[test]
    fn a_listing_keeps_at_most_max_scan_names() {
        let t = T::new();
        for i in 0..MAX_SCAN + 5 {
            t.put(&format!("{i:03}.torrent"), b"x");
        }
        let f = Folder::open(t.dir(), t.data.path()).unwrap();
        let (names, whole) = f.list().unwrap();
        assert_eq!((names.len(), whole), (MAX_SCAN, false));
        std::fs::remove_file(t.dir().join("000.torrent")).unwrap();
        for i in 1..6 {
            std::fs::remove_file(t.dir().join(format!("{i:03}.torrent"))).unwrap();
        }
        let (names, whole) = f.list().unwrap();
        assert_eq!((names.len(), whole), (MAX_SCAN - 1, true));
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
            let mut patch = serde_json::Map::new();
            patch.insert("folder".into(), w.folder.clone().into());
            assert!(<Watch as crate::settings::Section>::confirm(&patch).is_err());
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
            t.put(&long(0), b"FLOOR");
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
                t.put(&long(i), b"FLOOR");
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

        /// rv-765 B2: `failed/` turning into a link partway through a poll
        /// stops it at the first file that can't be put away, and the
        /// never-read-again set stays within its cap however many files
        /// are listed.
        #[tokio::test]
        async fn a_subfolder_problem_stops_the_poll_within_the_cap() {
            /// Swaps `failed/` for a link on the first file it's sent.
            struct Swapper<'a> {
                inner: Fake,
                dir: &'a Path,
                outside: &'a Path,
                swapped: std::sync::atomic::AtomicBool,
            }
            impl Sink for Swapper<'_> {
                async fn submit(&self, body: Vec<u8>) -> Result<Outcome, Closed> {
                    let failed = self.dir.join(FAILED);
                    if !self.swapped.swap(true, Ordering::SeqCst) {
                        std::fs::remove_dir(&failed).unwrap();
                        std::os::unix::fs::symlink(self.outside, &failed).unwrap();
                    }
                    self.inner.submit(body).await
                }
            }
            let t = T::new();
            let outside = tempfile::tempdir().unwrap();
            for i in 0..MAX_STUCK * 3 {
                t.put(&format!("m{i:02}.torrent"), b"FLOOR");
            }
            let s = Swapper {
                inner: Fake::default(),
                dir: t.dir(),
                outside: outside.path(),
                swapped: Default::default(),
            };
            let mut w = t.watcher(After::Move);
            assert_eq!(w.poll(&s).await.problem, None);
            let p = w.poll(&s).await;
            assert_eq!(p.problem, Some(Problem::SubfolderLink));
            assert_eq!((p.failed, calls(&s.inner)), (1, 1), "stopped at once");
            assert_eq!(w.stuck.len(), 1);
            assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
            for _ in 0..3 {
                assert_eq!(w.poll(&s).await.problem, Some(Problem::SubfolderLink));
                assert!(w.stuck.len() <= MAX_STUCK);
            }
            assert_eq!(calls(&s.inner), 1);
            // Fixed: the stuck file is put away, the rest taken, unread twice.
            std::fs::remove_file(t.dir().join(FAILED)).unwrap();
            for _ in 0..3 {
                w.poll(&s).await;
                assert!(w.stuck.len() <= MAX_STUCK);
            }
            assert_eq!(t.names(FAILED).len(), MAX_STUCK * 3);
            assert_eq!(calls(&s.inner), MAX_STUCK * 3);
        }

        /// rv-765 O1: a stuck file removed by hand is forgotten once a whole
        /// listing shows it gone, so it never counts toward the cap.
        #[tokio::test]
        async fn a_stuck_file_removed_by_hand_is_forgotten() {
            let t = T::new();
            let long = format!("{}.torrent", "r".repeat(244));
            t.put(&long, b"FLOOR");
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            settle(&mut w, &f).await;
            assert_eq!(w.stuck.len(), 1);
            std::fs::remove_file(t.dir().join(&long)).unwrap();
            w.poll(&f).await;
            assert!(w.stuck.is_empty());
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        fn junction(link: &Path, target: &Path) {
            let ok = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "mklink /J");
        }

        /// rv-765 I3: junctions named `*.torrent` stay where they are, never
        /// read, counted once, and never hold the watch, however many.
        #[tokio::test]
        async fn junctions_are_left_alone_and_never_hold_the_watch() {
            let t = T::new();
            let outside = tempfile::tempdir().unwrap();
            for i in 0..MAX_STUCK + 2 {
                junction(&t.dir().join(format!("j{i}.torrent")), outside.path());
            }
            t.put("a.torrent", b"fine");
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            let first = w.poll(&f).await;
            assert_eq!((first.failed, first.problem), (MAX_STUCK as u64 + 2, None));
            let p = w.poll(&f).await;
            assert_eq!((p.added, p.problem), (1, None));
            assert!(w.stuck.is_empty() && !w.halted);
            assert_eq!(t.top().len(), MAX_STUCK + 2, "the junctions stay");
            assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        }

        /// rv-765 I2: while a poll holds the folder, neither it nor `added\`
        /// can be renamed (so not swapped for a junction).
        #[test]
        fn the_folders_are_held_for_the_poll() {
            let t = T::new();
            let f = Folder::open(t.dir(), t.data.path()).unwrap();
            let moved = t.root.path().with_extension("moved");
            assert!(std::fs::rename(t.dir().join(ADDED), t.dir().join("x")).is_err());
            assert!(std::fs::rename(t.dir(), &moved).is_err());
            drop(f);
            std::fs::rename(t.dir().join(ADDED), t.dir().join("x")).unwrap();
        }

        /// rv-765 I1: a file held open without delete sharing is never
        /// linked into `added\`, however often its move is retried.
        #[tokio::test]
        async fn a_file_that_cant_be_removed_leaves_no_names_behind() {
            use std::os::windows::fs::OpenOptionsExt;
            let t = T::new();
            t.put("a.torrent", b"fine");
            let f = Fake::default();
            let mut w = t.watcher(After::Move);
            w.poll(&f).await;
            let held = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0x1)
                .open(t.dir().join("a.torrent"))
                .unwrap();
            for _ in 0..3 {
                w.poll(&f).await;
            }
            assert!(t.names(ADDED).is_empty(), "{:?}", t.names(ADDED));
            drop(held);
            w.poll(&f).await;
            assert_eq!(t.names(ADDED), ["a.torrent"]);
        }
    }
}
