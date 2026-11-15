//! A running import's progress, beside the store (X-import-progress-visible).
//!
//! An import holds the store to itself (`open_exclusive`), so `status`
//! can't read the job from the store while it runs. The import writes
//! this small file instead: when it starts, after saved batches (at most
//! once a second), and on a heartbeat every [`HEARTBEAT`] while it waits
//! on a slow source. Each write goes to a new temporary file that's then
//! renamed, so a reader never sees half of one. The file is removed when
//! the import stops, a hard stop (the second Ctrl-C) included.
//!
//! `status` reads it only while the store is held, and only when it was
//! written in the last [`STALE_AFTER`]: a live import writes at least
//! every [`HEARTBEAT`], so an older file is one a crash left behind, and
//! whatever holds the store now is something else.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lodestar_store::import::ImportCounters;
use serde::{Deserialize, Serialize};

/// The file, in the data folder.
pub const FILE: &str = "import-progress.json";
/// How often a running import rewrites the file even with no new batch.
pub const HEARTBEAT: Duration = Duration::from_secs(15);
/// Older than this, the file is ignored (eight missed heartbeats).
pub const STALE_AFTER: Duration = Duration::from_secs(120);
/// The most often a saved batch is written.
const EVERY: Duration = Duration::from_secs(1);
/// A file longer than this isn't one this build wrote.
const MAX_BYTES: u64 = 64 * 1024;

/// What the file holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub job: i64,
    pub source: String,
    /// When this run started (unix seconds).
    pub run_started_at: i64,
    /// When the file was last written: the last batch or heartbeat.
    pub updated_at: i64,
    pub read: u64,
    pub new: u64,
    pub merged: u64,
    pub candidates: u64,
    pub filtered: u64,
    pub quarantined: u64,
    pub invalid: u64,
    pub files_dropped: u64,
    pub partial: u64,
    pub source_bytes: u64,
}

impl Progress {
    fn counters(&mut self, c: &ImportCounters) {
        self.read = c.read;
        self.new = c.new;
        self.merged = c.merged;
        self.candidates = c.candidates;
        self.filtered = c.filtered;
        self.quarantined = c.quarantined;
        self.invalid = c.invalid;
        self.files_dropped = c.files_dropped;
        self.partial = c.partial;
        self.source_bytes = c.source_bytes;
    }
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

fn tmp_path(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{FILE}.tmp"))
}

/// Removes the file and any temporary one; what a hard stop calls, as
/// [`Writer`]'s drop can't run then.
pub fn remove(data_dir: &Path) {
    let _ = std::fs::remove_file(path(data_dir));
    let _ = std::fs::remove_file(tmp_path(data_dir));
}

/// Writes a job's progress while it runs, and removes the file when
/// dropped. Writing is best effort: a failure costs only the live view.
#[derive(Debug)]
pub struct Writer {
    data_dir: PathBuf,
    progress: Progress,
    last: Option<Instant>,
}

impl Writer {
    /// Starts the file for `job`, replacing any a crashed import left.
    pub fn start(data_dir: &Path, job: i64, source: &str, now: i64, c: &ImportCounters) -> Writer {
        let mut progress = Progress {
            job,
            source: source.to_string(),
            run_started_at: now,
            updated_at: now,
            read: 0,
            new: 0,
            merged: 0,
            candidates: 0,
            filtered: 0,
            quarantined: 0,
            invalid: 0,
            files_dropped: 0,
            partial: 0,
            source_bytes: 0,
        };
        progress.counters(c);
        let mut w = Writer {
            data_dir: data_dir.to_path_buf(),
            progress,
            last: None,
        };
        w.write(now);
        w
    }

    /// The job's counters after a saved batch: written at most once a
    /// second; a batch held back goes out with the next write.
    pub fn saved(&mut self, c: &ImportCounters, now: i64) {
        #[cfg(test)]
        tests::count(&self.data_dir, "saved");
        self.progress.counters(c);
        if self.last.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.write(now);
    }

    /// Rewrites the file with what's known, batch or not, so a slow source
    /// doesn't make a live import look gone.
    pub fn heartbeat(&mut self, now: i64) {
        self.write(now);
    }

    fn write(&mut self, now: i64) {
        self.last = Some(Instant::now());
        self.progress.updated_at = now;
        let Ok(text) = serde_json::to_vec(&self.progress) else {
            return;
        };
        let tmp = tmp_path(&self.data_dir);
        // A new file each time, never through whatever sits at the name.
        let _ = std::fs::remove_file(&tmp);
        let written = create(&tmp).and_then(|mut f| std::io::Write::write_all(&mut f, &text));
        if written.is_err() || std::fs::rename(&tmp, path(&self.data_dir)).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        #[cfg(test)]
        tests::count(&self.data_dir, "written");
    }
}

/// A new file at `path`, never through a link at the name. Readable by
/// all on unix, so `status` run as another user can read it: it holds
/// counts only.
#[cfg(unix)]
fn create(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(crate::update::layout::nofollow())
        .open(path)
}

#[cfg(not(unix))]
fn create(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

impl Drop for Writer {
    fn drop(&mut self) {
        remove(&self.data_dir);
    }
}

/// The running import's progress, if the file is there, readable and
/// written no longer than [`STALE_AFTER`] before `now`. Anything else
/// reads as none.
pub fn read(data_dir: &Path, now: i64) -> Option<Progress> {
    let mut text = Vec::new();
    // Not through a link, never blocking on a FIFO, a regular file only.
    crate::update::layout::open_nofollow(&path(data_dir))
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_BYTES {
        return None;
    }
    let progress: Progress = serde_json::from_slice(&text).ok()?;
    // A minute's leeway for a clock that stepped back.
    let age = now.saturating_sub(progress.updated_at);
    (-60..=STALE_AFTER.as_secs() as i64)
        .contains(&age)
        .then_some(progress)
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    type Counts = HashMap<(PathBuf, &'static str), u64>;
    static COUNTS: Mutex<Option<Counts>> = Mutex::new(None);

    pub(super) fn count(data_dir: &Path, what: &'static str) {
        let mut c = COUNTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *c.get_or_insert_with(HashMap::new)
            .entry((data_dir.to_path_buf(), what))
            .or_default() += 1;
    }

    /// How many times `what` ("written", "saved") happened for the file
    /// in `data_dir`.
    pub(in crate::cli::import) fn counted(data_dir: &Path, what: &'static str) -> u64 {
        let c = COUNTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        c.as_ref()
            .and_then(|m| m.get(&(data_dir.to_path_buf(), what)).copied())
            .unwrap_or(0)
    }

    fn counters(read: u64) -> ImportCounters {
        ImportCounters {
            read,
            new: read / 2,
            source_bytes: read * 10,
            ..ImportCounters::default()
        }
    }

    /// The writer starts the file, writes saved batches at most once a
    /// second (a held-back one goes out with the next write), beats while
    /// no batch lands, and removes it when dropped.
    #[test]
    fn the_file_is_written_throttled_beaten_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer::start(dir.path(), 3, "magnetico", 1_000, &counters(500));
        let p = read(dir.path(), 1_000).expect("written at start");
        assert_eq!(
            (p.job, p.source.as_str(), p.read, p.new, p.source_bytes),
            (3, "magnetico", 500, 250, 5_000)
        );
        // Within the second: kept in memory, not written.
        w.saved(&counters(1_000), 1_001);
        assert_eq!(read(dir.path(), 1_001).unwrap().read, 500);
        // A heartbeat writes it, with its own time.
        w.heartbeat(1_030);
        let p = read(dir.path(), 1_030).unwrap();
        assert_eq!(
            (p.read, p.updated_at, p.run_started_at),
            (1_000, 1_030, 1_000)
        );
        // Once a second has passed, a batch is written.
        w.last = w.last.map(|t| t - EVERY);
        w.saved(&counters(1_500), 1_031);
        assert_eq!(read(dir.path(), 1_031).unwrap().read, 1_500);
        assert!(!tmp_path(dir.path()).exists());
        drop(w);
        assert!(!path(dir.path()).exists(), "removed when the run ends");
        assert_eq!(read(dir.path(), 1_031), None);
    }

    /// Only a recent, well-formed file of a sane size is read, and a hard
    /// stop's removal clears it.
    #[test]
    fn a_stale_oversized_or_garbled_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let w = Writer::start(dir.path(), 1, "bitmagnet", 1_000, &counters(0));
        let stale = 1_000 + STALE_AFTER.as_secs() as i64;
        assert!(read(dir.path(), stale).is_some(), "at the limit");
        assert_eq!(read(dir.path(), stale + 1), None, "past it");
        assert!(
            read(dir.path(), 1_000 - 60).is_some(),
            "a clock a minute behind"
        );
        assert_eq!(read(dir.path(), 1_000 - 61), None);
        // A hard stop: no drop, the file removed directly.
        std::mem::forget(w);
        remove(dir.path());
        assert_eq!(read(dir.path(), 1_000), None);
        std::fs::write(path(dir.path()), b"{not json").unwrap();
        assert_eq!(read(dir.path(), 1_000), None);
        std::fs::write(path(dir.path()), vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(read(dir.path(), 1_000), None);
    }

    /// Anything but a regular file at the name reads as none: a directory
    /// everywhere, and on unix a link to a good file and a FIFO, which
    /// mustn't hang the read.
    #[test]
    fn only_a_regular_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(path(dir.path())).unwrap();
        assert_eq!(read(dir.path(), 1_000), None);
        std::fs::remove_dir(path(dir.path())).unwrap();
        #[cfg(unix)]
        {
            let elsewhere = tempfile::tempdir().unwrap();
            let w = Writer::start(elsewhere.path(), 1, "magnetico", 1_000, &counters(1));
            assert!(read(elsewhere.path(), 1_000).is_some());
            std::os::unix::fs::symlink(path(elsewhere.path()), path(dir.path())).unwrap();
            assert_eq!(read(dir.path(), 1_000), None, "a link isn't followed");
            drop(w);
            std::fs::remove_file(path(dir.path())).unwrap();
            rustix::fs::mknodat(
                rustix::fs::CWD,
                path(dir.path()),
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_raw_mode(0o644),
                0,
            )
            .unwrap();
            assert_eq!(read(dir.path(), 1_000), None, "a FIFO doesn't hang");
        }
    }

    /// The file is readable by other users on unix (counts only).
    #[cfg(unix)]
    #[test]
    fn the_file_is_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _w = Writer::start(dir.path(), 1, "magnetico", 1_000, &counters(1));
        let mode = std::fs::metadata(path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777 & !0o022, 0o644 & !0o022);
        assert_ne!(mode & 0o044, 0, "readable beyond the owner");
    }
}
