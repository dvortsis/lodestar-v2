//! The log file (§12.1): `<folder>/lodestar.jsonl`, rotated by size (and
//! by age, so no file outlives the maximum age by much), rotated files
//! gzip-compressed, at most so many kept, none older than the maximum age,
//! and the folder never over its total cap.
//!
//! - **Writers never wait on compression.** Rotating is two renames' worth
//!   of work under the caller's lock; compressing and pruning run on one
//!   background thread.
//! - **A failed rotation keeps appending** to the current file rather than
//!   dropping lines, and is retried a minute later.
//! - **Disk full** is reported to the caller, which stops file logging.
//! - **The details index** (`index.json` beside the files) says which files
//!   were written while an identifying kind or an opt-in was on, so B-log2
//!   can badge them and delete "only those with details".

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use super::format::Stamp;
use super::names::LogName;
use super::state::FileConfig;

/// The current file's name.
pub const CURRENT: &str = "lodestar.jsonl";
/// The details index's name.
pub const INDEX: &str = "index.json";
/// How long after a failed rotation the next one is tried.
const ROTATION_RETRY: Duration = Duration::from_secs(60);

/// What a file was written under.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags {
    /// An identifying kind or an opt-in was on: "contains torrent details".
    pub details: bool,
    /// An opt-in was on: never in diagnostics downloads.
    pub opt_in: bool,
}

impl Flags {
    fn union(self, o: Flags) -> Flags {
        Flags {
            details: self.details || o.details,
            opt_in: self.opt_in || o.opt_in,
        }
    }
}

/// `index.json`: the files with a flag set, by name. The API reads it
/// apart from this, as untrusted input (`files.rs`).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub files: BTreeMap<String, Flags>,
    /// When the run writing here was last alive (Unix seconds).
    #[serde(default)]
    pub alive_at: i64,
    /// Lodestar made this folder and restricted its ACL (Windows). A hint
    /// only: whether a folder is private is its owner's and DACL's to say
    /// (`windows::acl::log_folder`), since anyone who can write here could
    /// plant `true`. A rebuilt index doesn't know (`None`), and `false` is
    /// never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restricted: Option<bool>,
    #[serde(skip)]
    path: PathBuf,
}

impl Index {
    /// The folder's index. One that exists but can't be read is rebuilt
    /// failing closed: every log file in the folder has details and an
    /// opt-in, so the next save never drops a badge it can't vouch for.
    pub(super) fn load(dir: &Path) -> Index {
        let path = dir.join(INDEX);
        let read = match fs::read(&path) {
            Ok(b) => serde_json::from_slice::<Index>(&b).ok(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some(Index::default()),
            Err(_) => None,
        };
        let mut index = match read {
            Some(i) => i,
            None => {
                let mut i = Index::default();
                let all = Flags {
                    details: true,
                    opt_in: true,
                };
                for e in fs::read_dir(dir)
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                {
                    if let Some(name) = e.file_name().to_str().and_then(LogName::parse) {
                        i.files.insert(name.file_name(), all);
                    }
                }
                i
            }
        };
        index.path = path;
        index
    }

    /// A file's flags, with those of its `.jsonl`/`.gz` other self.
    pub(super) fn flags(&self, name: &LogName) -> Flags {
        let get = |n: &LogName| self.files.get(&n.file_name()).copied().unwrap_or_default();
        match name.sibling() {
            Some(other) => get(name).union(get(&other)),
            None => get(name),
        }
    }

    /// Gives `to` the flags of `from` too, before a file moves from one
    /// name to the other: no reader ever sees the file under a name
    /// without them.
    fn copy(&mut self, from: &str, to: &str) -> io::Result<()> {
        match self.files.get(from).copied() {
            Some(f) => self.set(to, f),
            None => Ok(()),
        }
    }

    fn save(&self) -> io::Result<()> {
        let tmp = self.path.with_extension("json.part");
        private(OpenOptions::new().write(true).create(true).truncate(true))
            .open(&tmp)?
            .write_all(&serde_json::to_vec(self).expect("an index serializes"))?;
        fs::rename(&tmp, &self.path)
    }

    fn set(&mut self, name: &str, flags: Flags) -> io::Result<()> {
        let old = self.files.get(name).copied().unwrap_or_default();
        let new = old.union(flags);
        if new == old {
            return Ok(());
        }
        self.files.insert(name.to_string(), new);
        self.save()
    }

    pub(super) fn remove(&mut self, name: &str) -> io::Result<()> {
        match self.files.remove(name) {
            Some(_) => self.save(),
            None => Ok(()),
        }
    }
}

/// Why a line wasn't written.
#[derive(Debug)]
pub enum WriteError {
    /// No space (or quota) left: file logging should stop.
    DiskFull,
    /// The current file reached the folder's total cap (rotation keeps
    /// failing); `first` the first line refused.
    OverCap {
        first: bool,
    },
    Other(io::Error),
}

pub fn disk_full(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::StorageFull || e.kind() == io::ErrorKind::QuotaExceeded {
        return true;
    }
    // ENOSPC and EDQUOT; ERROR_HANDLE_DISK_FULL and ERROR_DISK_FULL.
    #[cfg(unix)]
    let codes = [28, 122];
    #[cfg(windows)]
    let codes = [39, 112];
    #[cfg(not(any(unix, windows)))]
    let codes: [i32; 0] = [];
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

pub(super) enum Job {
    Compress(String, FileConfig),
    Prune(FileConfig),
    /// Deletes these files (and each one's `.gz`, should a compression
    /// have finished meanwhile), the `.part` leftovers, and their index
    /// entries, in turn with compression and pruning.
    Delete {
        dir: PathBuf,
        names: Vec<LogName>,
        take: Take,
        reply: mpsc::Sender<Deleted>,
    },
    #[cfg(test)]
    Barrier(mpsc::Sender<()>),
    /// Holds the thread until the sender sends or is dropped (30 s at
    /// most), so a test can queue jobs behind it.
    #[cfg(test)]
    Hold(mpsc::Receiver<()>),
}

/// The background thread that compresses and prunes.
#[derive(Debug)]
struct Worker {
    tx: mpsc::Sender<Job>,
}

impl Worker {
    fn start(index: Arc<Mutex<Index>>) -> Worker {
        let (tx, rx) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("log-rotate".into())
            .spawn(move || {
                for job in rx {
                    let result = match job {
                        // Pruned even when compressing failed: the cap holds
                        // either way.
                        Job::Compress(name, cfg) => {
                            let compressed = compress(&cfg.dir, &name, &index);
                            let pruned = prune(&cfg, &index, SystemTime::now());
                            compressed.and(pruned)
                        }
                        Job::Prune(cfg) => prune(&cfg, &index, SystemTime::now()),
                        Job::Delete {
                            dir,
                            names,
                            take,
                            reply,
                        } => {
                            let _ = reply.send(delete(&dir, &names, &take, &index));
                            Ok(())
                        }
                        #[cfg(test)]
                        Job::Barrier(done) => {
                            let _ = done.send(());
                            Ok(())
                        }
                        #[cfg(test)]
                        Job::Hold(until) => {
                            let _ = until.recv_timeout(Duration::from_secs(30));
                            Ok(())
                        }
                    };
                    if let Err(e) = result {
                        lodestar_log::log_event!(
                            super::events::LOGGING_CLEANUP_FAILED,
                            error = lodestar_log::Error::io(&e)
                        );
                    }
                }
            });
        if spawned.is_err() {
            // No thread: rotated files stay uncompressed until a later
            // start compresses them; nothing is lost.
            let (tx, _) = mpsc::channel();
            return Worker { tx };
        }
        Worker { tx }
    }

    fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

/// What a deletion did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Deleted {
    pub deleted: u64,
    pub bytes: u64,
    /// Name and reason: `not_a_regular_file` or `failed`.
    pub skipped: Vec<(String, &'static str)>,
}

/// What a deletion takes beyond the names it's given, chosen when the
/// job runs, from the folder and the index as they are then.
#[derive(Debug, Clone)]
pub(super) enum Take {
    /// "All": every rotated file not newer than `upto` (every one,
    /// without), so a rotation between the listing and the job leaves
    /// nothing older behind.
    All { upto: Option<LogName> },
    /// "Details": every rotated file whose flags (with its other self's)
    /// say details or an opt-in, so a file whose badge moved with it
    /// since the listing is still taken.
    Details,
}

/// Deletes `names` in `dir` (each one's `.gz` too), and what `take`
/// takes, never following a link: one is skipped and reported. Then the
/// `.part` leftovers, and the names' index entries.
pub(super) fn delete(dir: &Path, names: &[LogName], take: &Take, index: &Mutex<Index>) -> Deleted {
    let mut out = Deleted::default();
    let mut names = names.to_vec();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.filter_map(Result::ok) {
            let Some(name) = e.file_name().to_str().and_then(LogName::parse) else {
                continue;
            };
            let taken = match (&name, take) {
                (LogName::Current, _) => false,
                (_, Take::All { upto: None }) => true,
                (n, Take::All { upto: Some(upto) }) => {
                    LogName::newest_first(n, upto) != std::cmp::Ordering::Less
                }
                (n, Take::Details) => {
                    let f = lock(index).flags(n);
                    f.details || f.opt_in
                }
            };
            if taken && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    for name in &names {
        for n in std::iter::once(name.clone()).chain(name.compressed()) {
            let file = n.file_name();
            let path = n.path(dir);
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.file_type().is_file() {
                out.skipped.push((file, "not_a_regular_file"));
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => {
                    out.deleted += 1;
                    out.bytes += meta.len();
                    let _ = lock(index).remove(&file);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(_) => out.skipped.push((file, "failed")),
            }
        }
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            // Not `index.json.part`: the index's own save owns it.
            let leftover = name.starts_with("lodestar-") && name.ends_with(".part");
            if leftover && e.file_type().is_ok_and(|t| t.is_file()) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    out
}

/// The rotate worker and the index, for jobs from outside the writer.
#[derive(Debug, Clone)]
pub(super) struct Jobs {
    tx: mpsc::Sender<Job>,
    pub dir: PathBuf,
    /// The writer's own index, so a change from outside it isn't undone
    /// by its next save.
    pub index: Arc<Mutex<Index>>,
}

impl Jobs {
    /// Runs a deletion on the worker and waits for it, however long the
    /// compressions queued before it take: it never runs beside them.
    /// `Err(None)`: there's no worker, so the deletion may run elsewhere.
    /// `Err(Some(_))`: the worker took the job and ended without an answer.
    pub fn delete(&self, names: Vec<LogName>, take: Take) -> Result<Deleted, Option<io::Error>> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Job::Delete {
                dir: self.dir.clone(),
                names,
                take,
                reply,
            })
            .map_err(|_| None)?;
        rx.recv()
            .map_err(|_| Some(io::Error::other("the log-rotate worker ended")))
    }
}

/// The current log file and its rotation.
#[derive(Debug)]
pub struct FileSink {
    cfg: FileConfig,
    path: PathBuf,
    file: File,
    size: u64,
    opened: SystemTime,
    index: Arc<Mutex<Index>>,
    /// What the current file was written under, as the index has it.
    flags: Flags,
    worker: Worker,
    /// No rotation before this, after one failed.
    retry_at: Option<Instant>,
    /// The index's `alive_at` when this opened: the previous run's.
    last_alive: i64,
    /// The folder isn't Lodestar's own and isn't private.
    pub shared: bool,
    /// Lines refused because the current file reached the total cap.
    over_cap: bool,
    #[cfg(test)]
    pub fail_rotation: bool,
    /// Every write fails as with a full disk.
    #[cfg(test)]
    pub full: bool,
}

/// Log files are the owner's only (0600 on Unix): with an identifying
/// kind or an opt-in on, they hold torrent details.
pub(super) fn private(o: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never through a symlink someone planted in the folder.
        o.mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    o
}

/// The file's mode 0600 whatever it was created with (an older file, a
/// wider umask).
fn tighten(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
    }
    let _ = file;
    Ok(())
}

/// How private the logs folder is, once checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    /// Lodestar's own and 0700 (or restricted by ACL on Windows).
    Private,
    /// Another account's: left as it is, with a warning.
    Shared,
}

/// Makes the folder (0700), tightens it when it's Lodestar's own, and
/// refuses one anyone can write to without the sticky bit, where another
/// account could replace the files.
fn folder(dir: &Path) -> io::Result<Folder> {
    // Whether Lodestar made the folder just now, from the creation itself
    // (a check before it could miss one planted in between).
    #[cfg(windows)]
    let existed = {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::create_dir(dir) {
            Ok(()) => false,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => true,
            Err(e) => return Err(e),
        }
    };
    #[cfg(not(windows))]
    {
        let mut dirs = fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut dirs, 0o700);
        dirs.create(dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::metadata(dir)?;
        let mode = meta.mode();
        if meta.uid() == rustix::process::geteuid().as_raw() {
            if mode & 0o077 != 0 {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
            return Ok(Folder::Private);
        }
        if mode & 0o002 != 0 && mode & 0o1000 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the logs folder is writable by anyone",
            ));
        }
        Ok(if mode & 0o077 == 0 {
            Folder::Private
        } else {
            Folder::Shared
        })
    }
    #[cfg(windows)]
    {
        // As on Unix, an existing folder keeps its ACL: restricting a
        // chosen `D:\Downloads` would take Users' access to everything in
        // it. Its owner and DACL say whether it's private, never the
        // index's `restricted` hint, which anyone who can write here could
        // plant, and which a corrupt index loses.
        if existed {
            return Ok(match crate::windows::acl::log_folder(dir) {
                Ok(()) => Folder::Private,
                Err(_) => Folder::Shared,
            });
        }
        // A folder Lodestar just made holds the first-run setup token:
        // only its owner, SYSTEM and Administrators may read it. Failing
        // that, file logging stays off, and a service operator, who sees no
        // console, finds the reason beside the service's own.
        if let Err(e) = lodestar_integrations::secret::restrict_dir_acl(dir) {
            // Removed again (it's empty), so the next start doesn't take
            // it for an existing folder and log into it unrestricted.
            let _ = fs::remove_dir(dir);
            service_log(&format!(
                "file logging is off: the new logs folder's access couldn't be restricted ({e})"
            ));
            return Err(e);
        }
        let mut index = Index::load(dir);
        index.restricted = Some(true);
        index.save()?;
        Ok(Folder::Private)
    }
    #[cfg(not(any(unix, windows)))]
    Ok(Folder::Shared)
}

/// Appends a line to `%SystemRoot%\Temp\lodestar-service.log`, where the
/// Windows service and its installer write the reasons a service operator
/// needs, never through a planted link (`windows::temp_log`). The line
/// names no path.
#[cfg(windows)]
fn service_log(why: &str) {
    let _ = crate::windows::temp_log::append("logging", why);
}

fn last_byte(path: &Path) -> io::Result<Option<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut f = File::open(path)?;
    if f.seek(SeekFrom::End(0))? == 0 {
        return Ok(None);
    }
    f.seek(SeekFrom::End(-1))?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    Ok(Some(b[0]))
}

fn open_append(path: &Path) -> io::Result<File> {
    let file = private(OpenOptions::new().create(true).append(true)).open(path)?;
    tighten(&file)?;
    Ok(file)
}

impl FileSink {
    /// Opens (or creates) the current file in `cfg.dir`, compresses
    /// rotated files a previous run left uncompressed, and prunes.
    pub fn open(cfg: FileConfig) -> io::Result<FileSink> {
        let shared = folder(&cfg.dir)? == Folder::Shared;
        let path = cfg.dir.join(CURRENT);
        let index = Arc::new(Mutex::new(Index::load(&cfg.dir)));
        let worker = Worker::start(index.clone());
        let file = open_append(&path)?;
        let meta = file.metadata()?;
        let flags = lock(&index).files.get(CURRENT).copied().unwrap_or_default();
        let last_alive = lock(&index).alive_at;
        let mut sink = FileSink {
            path,
            size: meta.len(),
            opened: SystemTime::now(),
            file,
            index,
            flags,
            worker,
            retry_at: None,
            last_alive,
            shared,
            over_cap: false,
            cfg,
            #[cfg(test)]
            fail_rotation: false,
            #[cfg(test)]
            full: false,
        };
        // A line a full disk cut short last time ends here, so the next
        // starts on its own.
        if sink.size > 0 && last_byte(&sink.path)? != Some(b'\n') {
            sink.file.write_all(b"\n")?;
            sink.size += 1;
        }
        for name in rotated(&sink.cfg.dir)? {
            if name.ends_with(".jsonl") {
                sink.worker.send(Job::Compress(name, sink.cfg.clone()));
            }
        }
        // A file left from long ago starts its own rotated file.
        let old = meta.modified().ok().and_then(|m| m.elapsed().ok());
        if sink.size > 0 && old.is_some_and(|age| age.as_secs() >= sink.cfg.max_age_secs) {
            let _ = sink.rotate();
        }
        sink.worker.send(Job::Prune(sink.cfg.clone()));
        Ok(sink)
    }

    pub(super) fn jobs(&self) -> Jobs {
        Jobs {
            tx: self.worker.tx.clone(),
            dir: self.cfg.dir.clone(),
            index: self.index.clone(),
        }
    }

    /// The previous run's last heartbeat (0 unknown).
    pub fn last_alive(&self) -> i64 {
        self.last_alive
    }

    /// Notes in the index that this run is alive at `now`.
    pub fn alive(&mut self, now: i64) {
        let mut index = lock(&self.index);
        if index.alive_at != now {
            index.alive_at = now;
            let _ = index.save();
        }
    }

    /// New size, count, age and cap limits (the folder stays until a
    /// restart), applied at the next write and prune.
    pub fn set_limits(&mut self, cfg: &FileConfig) {
        let cfg = FileConfig {
            dir: self.cfg.dir.clone(),
            ..cfg.clone()
        };
        if cfg != self.cfg {
            self.cfg = cfg;
            self.worker.send(Job::Prune(self.cfg.clone()));
        }
    }

    /// Appends one line (a newline is added). A rotation failure is
    /// returned beside a successful write: the line went to the current
    /// file.
    pub fn write(&mut self, line: &str, flags: Flags) -> Result<Option<io::Error>, WriteError> {
        let len = line.len() as u64 + 1;
        let mut rotation = None;
        if self.due(len) {
            if let Err(e) = self.rotate() {
                self.retry_at = Some(Instant::now() + ROTATION_RETRY);
                rotation = Some(e);
            }
        }
        // A rotation that keeps failing mustn't let one file grow past the
        // folder's cap: lines stop until a rotation succeeds.
        if self.size + len > self.cfg.total {
            let first = !self.over_cap;
            self.over_cap = true;
            return Err(WriteError::OverCap { first });
        }
        self.over_cap = false;
        #[cfg(test)]
        if self.full {
            return Err(WriteError::DiskFull);
        }
        if (flags.details && !self.flags.details) || (flags.opt_in && !self.flags.opt_in) {
            self.flags = self.flags.union(flags);
            // Best effort: a lost index entry only loses a badge.
            let _ = lock(&self.index).set(CURRENT, self.flags);
        }
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        match self.file.write_all(&buf) {
            Ok(()) => {
                self.size += len;
                Ok(rotation)
            }
            Err(e) if disk_full(&e) => Err(WriteError::DiskFull),
            Err(e) => Err(WriteError::Other(e)),
        }
    }

    fn due(&self, len: u64) -> bool {
        if self.retry_at.is_some_and(|t| Instant::now() < t) || self.size == 0 {
            return false;
        }
        let too_old = self
            .opened
            .elapsed()
            .is_ok_and(|age| age.as_secs() >= self.cfg.max_age_secs);
        self.size + len > self.cfg.size || too_old
    }

    /// Renames the current file to `lodestar-<time>.jsonl`, starts a new
    /// one, and hands the old one to the background thread. Returns the
    /// rotated name.
    pub fn rotate(&mut self) -> io::Result<String> {
        self.rotate_then(true)
    }

    /// Rotates for a deletion: the rotated file isn't compressed, since
    /// the deletion queued behind it takes it.
    pub(super) fn rotate_to_delete(&mut self) -> io::Result<String> {
        self.rotate_then(false)
    }

    fn rotate_then(&mut self, compress: bool) -> io::Result<String> {
        #[cfg(test)]
        if self.fail_rotation {
            return Err(io::Error::other("rotation refused by the test"));
        }
        let name = rotated_name(&self.cfg.dir);
        let to = self.cfg.dir.join(&name);
        // Its badge goes first, so it's never under a name without one.
        let _ = lock(&self.index).copy(CURRENT, &name);
        #[cfg(test)]
        before_rename(&self.cfg.dir, &name);
        if let Err(e) = fs::rename(&self.path, &to) {
            let _ = lock(&self.index).remove(&name);
            return Err(e);
        }
        match open_append(&self.path) {
            Ok(f) => self.file = f,
            Err(e) => {
                // Put it back, so lines go on in the current file and a
                // prune never takes them. Its badge stays under the
                // rotated name too unless it's back.
                if fs::rename(&to, &self.path).is_ok() {
                    let _ = lock(&self.index).remove(&name);
                }
                return Err(e);
            }
        }
        self.size = 0;
        self.opened = SystemTime::now();
        self.flags = Flags::default();
        let _ = lock(&self.index).remove(CURRENT);
        self.retry_at = None;
        if compress {
            self.worker
                .send(Job::Compress(name.clone(), self.cfg.clone()));
        }
        Ok(name)
    }

    /// Waits for the background thread to finish what it was given.
    #[cfg(test)]
    pub fn settle(&self) {
        let (tx, rx) = mpsc::channel();
        self.worker.send(Job::Barrier(tx));
        let _ = rx.recv_timeout(Duration::from_secs(30));
    }

    /// Holds the background thread until the returned sender sends or is
    /// dropped: jobs sent meanwhile queue behind the hold.
    #[cfg(test)]
    pub fn hold(&self) -> mpsc::Sender<()> {
        let (tx, rx) = mpsc::channel();
        self.worker.send(Job::Hold(rx));
        tx
    }
}

/// A free rotated name in `dir` for a file set aside now.
fn rotated_name(dir: &Path) -> String {
    rotated_name_at(dir, &Stamp::now().compact())
}

/// A free rotated name in `dir` for `stamp`, numbered past every file
/// already there with that stamp. (Not the lowest free `-N`: after a prune
/// took `-1` but kept `-2`, a reused `-1` would sort as the oldest and go
/// first in the next prune.)
fn rotated_name_at(dir: &Path, stamp: &str) -> String {
    let taken = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| LogName::parse(e.ok()?.file_name().to_str()?))
        .filter_map(|name| match name {
            LogName::Rotated { stamp: s, n, .. } if s == stamp => Some(n),
            _ => None,
        })
        .max();
    let mut n = taken.map_or(1, |n| n + 1);
    loop {
        let name = match n {
            1 => format!("lodestar-{stamp}.jsonl"),
            n => format!("lodestar-{stamp}-{n}.jsonl"),
        };
        let gz = format!("{name}.gz");
        if !dir.join(&name).exists() && !dir.join(gz).exists() {
            return name;
        }
        n += 1;
    }
}

/// With no writer in `dir`, moves the current file aside under a rotated
/// name for a deletion, so a writer opening after this starts a new one
/// rather than writing to a file about to go. `Ok(None)`: there's none.
/// A link with its name is never moved.
pub(super) fn set_aside(dir: &Path, index: &Mutex<Index>) -> Result<Option<String>, &'static str> {
    let path = dir.join(CURRENT);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("failed"),
        Ok(m) if !m.file_type().is_file() => return Err("not_a_regular_file"),
        Ok(_) => {}
    }
    let name = rotated_name(dir);
    // Its badge moves with it, first.
    let _ = lock(index).copy(CURRENT, &name);
    #[cfg(test)]
    before_rename(dir, &name);
    match fs::rename(&path, dir.join(&name)) {
        Ok(()) => {
            let _ = lock(index).remove(CURRENT);
            Ok(Some(name))
        }
        Err(e) => {
            let _ = lock(index).remove(&name);
            match e.kind() {
                io::ErrorKind::NotFound => Ok(None),
                _ => Err("failed"),
            }
        }
    }
}

#[cfg(test)]
type RenameHook = Box<dyn Fn(&Path, &str)>;

#[cfg(test)]
thread_local! {
    /// Runs on this thread just before the current file is renamed to a
    /// rotated name (`dir`, the name), to check what a reader sees then.
    pub(super) static BEFORE_RENAME: std::cell::RefCell<Option<RenameHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn before_rename(dir: &Path, name: &str) {
    BEFORE_RENAME.with(|h| {
        if let Some(h) = &*h.borrow() {
            h(dir, name);
        }
    });
}

pub(super) fn lock(index: &Mutex<Index>) -> std::sync::MutexGuard<'_, Index> {
    index.lock().unwrap_or_else(|e| e.into_inner())
}

/// The rotated files in `dir`, newest first by what their names say: by
/// time, then `-N` as a number. (Not as strings: `lodestar-<t>.jsonl`
/// sorts after `lodestar-<t>-2.jsonl`, rotated later in the same
/// millisecond, and `-10` before `-2`, so a prune would keep an older
/// file over a newer one.)
fn rotated(dir: &Path) -> io::Result<Vec<String>> {
    let mut names: Vec<LogName> = fs::read_dir(dir)?
        .filter_map(|e| LogName::parse(e.ok()?.file_name().to_str()?))
        .filter(|n| *n != LogName::Current)
        .collect();
    names.sort_by(LogName::newest_first);
    Ok(names.iter().map(LogName::file_name).collect())
}

fn compress(dir: &Path, name: &str, index: &Mutex<Index>) -> io::Result<()> {
    let from = dir.join(name);
    let gz = format!("{name}.gz");
    let part = dir.join(format!("{gz}.part"));
    let input = File::open(&from)?;
    let mut out = io::BufWriter::new(
        private(OpenOptions::new().write(true).create(true).truncate(true)).open(&part)?,
    );
    // Members of about 1 MiB and their index, so history can start at the
    // member it needs (`read::write_gz`).
    super::read::write_gz(&mut io::BufReader::with_capacity(64 << 10, input), &mut out)?;
    out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    // The badge before the name: no reader sees the `.gz` without it.
    let _ = lock(index).copy(name, &gz);
    fs::rename(&part, dir.join(&gz))?;
    fs::remove_file(&from)?;
    let _ = lock(index).remove(name);
    Ok(())
}

/// Deletes rotated files past the count or the age, then the oldest until
/// the folder is within its cap.
fn prune(cfg: &FileConfig, index: &Mutex<Index>, now: SystemTime) -> io::Result<()> {
    let dir = &cfg.dir;
    // Leftovers of a compression that didn't finish.
    for e in fs::read_dir(dir)?.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("lodestar-") && name.ends_with(".part") {
            let _ = fs::remove_file(e.path());
        }
    }
    let mut kept = Vec::new();
    for (i, name) in rotated(dir)?.into_iter().enumerate() {
        let meta = match fs::metadata(dir.join(&name)) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        if i >= cfg.count || age.as_secs() >= cfg.max_age_secs {
            remove(dir, &name, index)?;
        } else {
            kept.push((name, meta.len()));
        }
    }
    let current = fs::metadata(dir.join(CURRENT)).map_or(0, |m| m.len());
    let mut total = current + kept.iter().map(|(_, n)| n).sum::<u64>();
    while total > cfg.total {
        let Some((name, len)) = kept.pop() else { break };
        remove(dir, &name, index)?;
        total -= len;
    }
    Ok(())
}

fn remove(dir: &Path, name: &str, index: &Mutex<Index>) -> io::Result<()> {
    match fs::remove_file(dir.join(name)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let _ = lock(index).remove(name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_index_is_rebuilt_failing_closed() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for name in [
            CURRENT,
            "lodestar-20261023T101500.123Z.jsonl.gz",
            "notes.txt",
        ] {
            fs::write(d.join(name), "x\n").unwrap();
        }
        fs::write(d.join(INDEX), "{not json").unwrap();
        let mut index = Index::load(d);
        let all = Flags {
            details: true,
            opt_in: true,
        };
        assert_eq!(index.files.len(), 2, "{:?}", index.files);
        assert!(index.files.values().all(|f| *f == all));
        // The next save keeps the badges: never an empty index.
        index.alive_at = 1;
        index.save().unwrap();
        let back = Index::load(d);
        assert_eq!(back.files.get(CURRENT), Some(&all));
        assert_eq!(
            back.files.get("lodestar-20261023T101500.123Z.jsonl.gz"),
            Some(&all)
        );
        // A missing one is an empty one.
        fs::remove_file(d.join(INDEX)).unwrap();
        assert!(Index::load(d).files.is_empty());
    }

    /// Sets a hook asserting that, on disk, the rotated name already has
    /// details when the current file is about to take it; returns how many
    /// times it ran.
    fn badge_first() -> std::rc::Rc<std::cell::Cell<u32>> {
        let ran = std::rc::Rc::new(std::cell::Cell::new(0));
        let r = ran.clone();
        BEFORE_RENAME.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |dir: &Path, name: &str| {
                let seen = super::super::files::read_index(dir);
                let f = seen.flags(&LogName::parse(name).unwrap());
                assert!(f.details, "{name} has no badge on disk before the rename");
                r.set(r.get() + 1);
            }));
        });
        ran
    }

    #[test]
    fn a_rotation_and_a_set_aside_write_the_badge_before_the_rename() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let details = Flags {
            details: true,
            opt_in: false,
        };
        let ran = badge_first();
        // Set aside, with no writer.
        let index = Mutex::new(Index::load(d));
        fs::write(d.join(CURRENT), "line\n").unwrap();
        lock(&index).set(CURRENT, details).unwrap();
        set_aside(d, &index).unwrap().unwrap();
        assert_eq!(ran.get(), 1);
        // A writer's rotation.
        let mut sink = FileSink::open(cfg(d, 1 << 20)).unwrap();
        sink.write("{\"msg\":\"x\"}", details).unwrap();
        assert!(
            lock(&sink.index)
                .files
                .get(CURRENT)
                .is_some_and(|f| f.details)
        );
        sink.rotate().unwrap();
        sink.settle();
        assert_eq!(ran.get(), 2);
        BEFORE_RENAME.with(|h| *h.borrow_mut() = None);
    }

    #[test]
    fn the_current_file_is_set_aside_never_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let index = Mutex::new(Index::load(d));
        assert_eq!(set_aside(d, &index), Ok(None), "none there");
        fs::write(d.join(CURRENT), "line\n").unwrap();
        let details = Flags {
            details: true,
            opt_in: false,
        };
        lock(&index).set(CURRENT, details).unwrap();
        let name = set_aside(d, &index).unwrap().unwrap();
        assert!(LogName::parse(&name).is_some_and(|n| n != LogName::Current));
        // Its badge went with it, on disk too.
        let on_disk = Index::load(d).files;
        assert_eq!(on_disk.get(&name), Some(&details));
        assert!(!on_disk.contains_key(CURRENT));
        assert!(!d.join(CURRENT).exists());
        assert_eq!(fs::read_to_string(d.join(&name)).unwrap(), "line\n");
        #[cfg(unix)]
        {
            let elsewhere = d.join("elsewhere");
            fs::write(&elsewhere, "x").unwrap();
            std::os::unix::fs::symlink(&elsewhere, d.join(CURRENT)).unwrap();
            assert_eq!(set_aside(d, &index), Err("not_a_regular_file"));
            assert!(d.join(CURRENT).is_symlink());
        }
    }

    fn cfg(dir: &Path, size: u64) -> FileConfig {
        FileConfig {
            dir: dir.to_path_buf(),
            size,
            count: 7,
            max_age_secs: 14 * 86_400,
            total: 256 << 20,
        }
    }

    /// Every line in the folder, rotated files decompressed.
    fn lines(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let text = if name.ends_with(".gz") {
                let mut s = String::new();
                flate2::read::GzDecoder::new(File::open(&p).unwrap())
                    .read_to_string(&mut s)
                    .unwrap();
                s
            } else if name.ends_with(".jsonl") {
                fs::read_to_string(&p).unwrap()
            } else {
                continue;
            };
            out.extend(text.lines().map(str::to_string));
        }
        out
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn rotates_at_size_and_gzips() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = FileSink::open(cfg(dir.path(), 100)).unwrap();
        for i in 0..10 {
            sink.write(
                &format!("{{\"n\":{i},\"pad\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}}"),
                Flags::default(),
            )
            .unwrap();
        }
        sink.settle();
        let ns = names(dir.path());
        assert!(
            ns.iter().filter(|n| n.ends_with(".jsonl.gz")).count() >= 3,
            "{ns:?}"
        );
        assert!(
            !ns.iter()
                .any(|n| n.starts_with("lodestar-") && n.ends_with(".jsonl")),
            "{ns:?}"
        );
        assert!(fs::metadata(dir.path().join(CURRENT)).unwrap().len() <= 100);
        let mut got: Vec<String> = lines(dir.path());
        got.sort();
        assert_eq!(got.len(), 10);
    }

    #[test]
    fn keeps_the_count_the_age_and_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path(), 50);
        c.count = 2;
        let mut sink = FileSink::open(c.clone()).unwrap();
        for i in 0..12 {
            sink.write(
                &format!("line {i} ........................................"),
                Flags::default(),
            )
            .unwrap();
            // Distinct rotated names even within one millisecond.
        }
        sink.settle();
        let rotated_now = rotated(dir.path()).unwrap();
        assert_eq!(rotated_now.len(), 2, "{rotated_now:?}");
        // The newest stay: the last lines written are still there.
        assert!(lines(dir.path()).iter().any(|l| l.starts_with("line 11")));

        // Age: an old rotated file goes at the next prune.
        let old = dir.path().join("lodestar-20000101T000000.000Z.jsonl.gz");
        fs::write(&old, b"x").unwrap();
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(15 * 86_400))
            .unwrap();
        let index = Mutex::new(Index::load(dir.path()));
        c.count = 10;
        prune(&c, &index, SystemTime::now()).unwrap();
        assert!(!old.exists());

        // Cap: the oldest go until the folder fits.
        let total: u64 = rotated(dir.path())
            .unwrap()
            .iter()
            .map(|n| fs::metadata(dir.path().join(n)).unwrap().len())
            .sum();
        c.total = fs::metadata(dir.path().join(CURRENT)).unwrap().len() + total - 1;
        prune(&c, &index, SystemTime::now()).unwrap();
        assert_eq!(rotated(dir.path()).unwrap().len(), 1);
    }

    #[test]
    fn the_count_keeps_the_newest_of_one_millisecond() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let t = "20261030T120000.000Z";
        // Oldest to newest; as strings, the first sorts after all but the
        // older millisecond, and `-10` before `-2`.
        let names = [
            "lodestar-20261030T115959.999Z.jsonl.gz".to_string(),
            format!("lodestar-{t}.jsonl.gz"),
            format!("lodestar-{t}-2.jsonl.gz"),
            format!("lodestar-{t}-10.jsonl"),
        ];
        for n in &names {
            fs::write(d.join(n), b"x\n").unwrap();
        }
        assert_eq!(
            rotated(d).unwrap(),
            names.iter().rev().cloned().collect::<Vec<_>>()
        );
        let mut c = cfg(d, 50);
        c.count = 2;
        prune(&c, &Mutex::new(Index::load(d)), SystemTime::now()).unwrap();
        assert_eq!(rotated(d).unwrap(), [names[3].clone(), names[2].clone()]);
    }

    #[test]
    fn a_rotated_name_never_reuses_a_number_below_a_kept_one() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let t = "20261030T120000.000Z";
        assert_eq!(rotated_name_at(d, t), format!("lodestar-{t}.jsonl"));
        // A prune took the first of the millisecond and kept `-2`: the
        // next is `-3`, so it sorts as the newest, not the oldest.
        fs::write(d.join(format!("lodestar-{t}-2.jsonl.gz")), b"x\n").unwrap();
        let next = rotated_name_at(d, t);
        assert_eq!(next, format!("lodestar-{t}-3.jsonl"));
        fs::write(d.join(&next), b"x\n").unwrap();
        assert_eq!(rotated(d).unwrap()[0], next);
        // Another millisecond's numbers don't count.
        assert_eq!(
            rotated_name_at(d, "20261030T120000.001Z"),
            "lodestar-20261030T120000.001Z.jsonl"
        );
    }

    #[test]
    fn concurrent_writers_lose_no_line_and_interleave_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path(), 4096);
        c.count = 10_000;
        let sink = Arc::new(Mutex::new(FileSink::open(c).unwrap()));
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let sink = sink.clone();
                std::thread::spawn(move || {
                    for i in 0..500 {
                        let line =
                            format!("{{\"t\":{t},\"i\":{i},\"pad\":\"{}\"}}", "y".repeat(i % 50));
                        sink.lock().unwrap().write(&line, Flags::default()).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        sink.lock().unwrap().settle();
        let mut seen = std::collections::BTreeSet::new();
        for l in lines(dir.path()) {
            let v: serde_json::Value =
                serde_json::from_str(&l).unwrap_or_else(|e| panic!("{e}: {l}"));
            assert!(
                seen.insert((v["t"].as_u64().unwrap(), v["i"].as_u64().unwrap())),
                "twice: {l}"
            );
        }
        // 8 × 500, minus whatever pruning took: none with these limits.
        assert_eq!(seen.len(), 4000);
    }

    #[test]
    fn the_details_index_follows_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = FileSink::open(cfg(dir.path(), 60)).unwrap();
        sink.write("plain line one", Flags::default()).unwrap();
        let read = || Index::load(dir.path()).files;
        assert!(read().is_empty());
        sink.write(
            "with details",
            Flags {
                details: true,
                opt_in: false,
            },
        )
        .unwrap();
        assert_eq!(
            read()[CURRENT],
            Flags {
                details: true,
                opt_in: false
            }
        );
        // Rotation: the flags move with the file, and the new one is clean.
        sink.write(&"z".repeat(70), Flags::default()).unwrap();
        sink.settle();
        let files = read();
        assert!(!files.contains_key(CURRENT), "{files:?}");
        let (name, flags) = files.iter().next().unwrap();
        assert!(name.ends_with(".jsonl.gz") && flags.details, "{files:?}");
        sink.write(
            "opt-in",
            Flags {
                details: true,
                opt_in: true,
            },
        )
        .unwrap();
        assert_eq!(
            read()[CURRENT],
            Flags {
                details: true,
                opt_in: true
            }
        );
    }

    #[test]
    fn a_failed_rotation_keeps_appending() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = FileSink::open(cfg(dir.path(), 20)).unwrap();
        sink.fail_rotation = true;
        sink.write("first line, long enough", Flags::default())
            .unwrap();
        let failed = sink
            .write("second line, long enough", Flags::default())
            .unwrap();
        assert!(failed.is_some(), "the failure is reported");
        // Not retried at once, and nothing dropped.
        assert!(
            sink.write("third line", Flags::default())
                .unwrap()
                .is_none()
        );
        assert_eq!(lines(dir.path()).len(), 3);
        sink.fail_rotation = false;
        sink.retry_at = None;
        sink.write("fourth line", Flags::default()).unwrap();
        sink.settle();
        assert_eq!(lines(dir.path()).len(), 4);
        assert_eq!(rotated(dir.path()).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn log_files_are_the_owners_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut sink = FileSink::open(cfg(dir.path(), 30)).unwrap();
        sink.write(
            "details line, long enough",
            Flags {
                details: true,
                opt_in: false,
            },
        )
        .unwrap();
        sink.write("and another to rotate it", Flags::default())
            .unwrap();
        sink.settle();
        for name in names(dir.path()) {
            let mode = fs::metadata(dir.path().join(&name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{name}: {mode:o}");
        }
    }

    #[test]
    fn disk_full_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = FileSink::open(cfg(dir.path(), 1 << 20)).unwrap();
        sink.full = true;
        assert!(matches!(
            sink.write("x", Flags::default()),
            Err(WriteError::DiskFull)
        ));
        assert!(disk_full(&io::Error::from(io::ErrorKind::StorageFull)));
        #[cfg(unix)]
        assert!(disk_full(&io::Error::from_raw_os_error(28)));
    }

    #[test]
    fn the_current_file_stops_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path(), 1 << 20);
        c.total = 40;
        let mut sink = FileSink::open(c).unwrap();
        sink.write("twenty bytes of line", Flags::default())
            .unwrap();
        assert!(matches!(
            sink.write("twenty bytes of line", Flags::default()),
            Err(WriteError::OverCap { first: true })
        ));
        assert!(matches!(
            sink.write("twenty bytes of line", Flags::default()),
            Err(WriteError::OverCap { first: false })
        ));
        assert!(fs::metadata(dir.path().join(CURRENT)).unwrap().len() <= 40);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_open_folders_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        fs::write(&target, "").unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir(&logs).unwrap();
        std::os::unix::fs::symlink(&target, logs.join(CURRENT)).unwrap();
        assert!(
            FileSink::open(cfg(&logs, 1 << 20)).is_err(),
            "never through a symlink"
        );
        // A loose folder of our own is tightened.
        let loose = dir.path().join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o777)).unwrap();
        let sink = FileSink::open(cfg(&loose, 1 << 20)).unwrap();
        assert!(!sink.shared);
        assert_eq!(
            fs::metadata(&loose).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // An existing loose file is tightened too.
        let older = dir.path().join("older");
        fs::create_dir(&older).unwrap();
        fs::write(older.join(CURRENT), "old line\n").unwrap();
        fs::set_permissions(older.join(CURRENT), fs::Permissions::from_mode(0o644)).unwrap();
        FileSink::open(cfg(&older, 1 << 20)).unwrap();
        assert_eq!(
            fs::metadata(older.join(CURRENT))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn leftovers_are_compressed_at_open() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("lodestar-20990101T000000.000Z.jsonl"),
            "left\n",
        )
        .unwrap();
        fs::write(
            dir.path()
                .join("lodestar-20990101T000000.000Z.jsonl.gz.part"),
            "junk",
        )
        .unwrap();
        let sink = FileSink::open(cfg(dir.path(), 1 << 20)).unwrap();
        sink.settle();
        let ns = names(dir.path());
        assert!(
            ns.contains(&"lodestar-20990101T000000.000Z.jsonl.gz".to_string()),
            "{ns:?}"
        );
        assert!(!ns.iter().any(|n| n.ends_with(".part")), "{ns:?}");
        assert_eq!(lines(dir.path()), ["left"]);
    }

    #[cfg(windows)]
    fn icacls(path: &Path, args: &[&str]) {
        let root = std::env::var_os("SystemRoot").unwrap();
        let out = std::process::Command::new(Path::new(&root).join("System32").join("icacls.exe"))
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "icacls {args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    /// The owner and the DACL decide "Private" on Windows, never the
    /// index's `restricted` flag.
    #[cfg(windows)]
    #[test]
    fn windows_privacy_is_the_acl_not_the_index() {
        let tmp = tempfile::tempdir().unwrap();

        // A shared folder (Users may read it) with `"restricted": true`
        // planted in its index is still shared.
        let shared = tmp.path().join("shared");
        fs::create_dir(&shared).unwrap();
        icacls(&shared, &["/grant", "*S-1-5-32-545:(OI)(CI)RX"]);
        fs::write(
            shared.join(INDEX),
            r#"{"files":{},"alive_at":0,"restricted":true}"#,
        )
        .unwrap();
        assert_eq!(folder(&shared).unwrap(), Folder::Shared);
        assert!(FileSink::open(cfg(&shared, 1 << 20)).unwrap().shared);

        // A folder Lodestar made is private; a corrupt index rebuilt and
        // saved doesn't make it shared, now or at the next start.
        let made = tmp.path().join("made");
        assert_eq!(folder(&made).unwrap(), Folder::Private);
        assert_eq!(Index::load(&made).restricted, Some(true));
        assert_eq!(crate::windows::acl::log_folder(&made), Ok(()));
        fs::write(made.join(INDEX), "{corrupt").unwrap();
        let rebuilt = Index::load(&made);
        assert_eq!(rebuilt.restricted, None, "unknown, never false");
        rebuilt.save().unwrap();
        assert!(
            !fs::read_to_string(made.join(INDEX))
                .unwrap()
                .contains("restricted")
        );
        assert_eq!(folder(&made).unwrap(), Folder::Private);
        let sink = FileSink::open(cfg(&made, 1 << 20)).unwrap();
        assert!(!sink.shared);
        drop(sink);
        assert!(!FileSink::open(cfg(&made, 1 << 20)).unwrap().shared);

        // The service's logs folder: made inside the private data folder
        // (as `windows::logfile` makes it), inheriting its ACL.
        let data = tmp.path().join("data");
        fs::create_dir(&data).unwrap();
        let own = crate::windows::acl::process_sid().expect("whoami");
        let grant = format!("*{own}:(OI)(CI)F");
        icacls(
            &data,
            &[
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
                &grant,
            ],
        );
        let log = crate::windows::logfile::path(&data);
        let logs = log.parent().unwrap();
        fs::create_dir_all(logs).unwrap();
        assert_eq!(crate::windows::acl::log_folder(logs), Ok(()));
        assert_eq!(folder(logs).unwrap(), Folder::Private);
    }
}
