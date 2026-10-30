//! Storage size gauges (R-gauges, owner's ruling on #189): the database,
//! its WAL and the search index on disk, so a disk or memory ramp in a
//! soak can be told apart from the caches.
//!
//! A background task stats the files every [`INTERVAL`] off the async
//! workers; `/metrics` reads the last values and never touches the
//! filesystem. Sampling is file-level only: no query, no connection, no
//! lock the writer or the indexer holds. A failed part keeps its last
//! value and counts an error.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

pub const INTERVAL: Duration = Duration::from_secs(60);

/// Entries the index walk visits before it gives up (an error, the last
/// value kept). A Tantivy index has hundreds of files.
pub const MAX_ENTRIES: usize = 100_000;

/// What is sampled.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The main SQLite file; its WAL is beside it as `<name>-wal`.
    pub db: PathBuf,
    /// The index root (`<data>/index`), every version and any rebuild in
    /// it; absent with search off.
    pub index: PathBuf,
}

/// The last sampled values.
#[derive(Debug, Default)]
pub struct Gauges {
    pub db_bytes: AtomicU64,
    pub wal_bytes: AtomicU64,
    pub index_bytes: AtomicU64,
    /// Unix time of the last sample in which every part succeeded.
    pub sampled_at: AtomicU64,
    /// Parts that failed, one per part per sample.
    pub errors: AtomicU64,
}

/// One sample: each part on its own, so one failure keeps only its value.
pub fn sample(paths: &Paths, g: &Gauges, max_entries: usize) {
    let wal = wal_path(&paths.db);
    let parts: [(io::Result<u64>, &AtomicU64); 3] = [
        (file_len(&paths.db, false), &g.db_bytes),
        (file_len(&wal, true), &g.wal_bytes),
        (dir_bytes(&paths.index, max_entries), &g.index_bytes),
    ];
    let mut ok = true;
    for (got, gauge) in parts {
        match got {
            Ok(v) => gauge.store(v, Ordering::Relaxed),
            Err(_) => {
                ok = false;
                g.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if ok {
        // Release: a reader that sees the time sees the sizes too.
        g.sampled_at
            .store(crate::unix_now().max(0) as u64, Ordering::Release);
    }
}

fn wal_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

/// A file's length; 0 when `missing_ok` and it doesn't exist. These are
/// fixed paths, so a database moved elsewhere behind a symlink is
/// followed and its own size reported.
fn file_len(path: &Path, missing_ok: bool) -> io::Result<u64> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(m.len()),
        Err(e) if missing_ok && e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

/// Bytes of the regular files under `root`, symlinks not followed, at
/// most `max_entries` entries visited. A missing root is 0 (search off);
/// a file or directory removed during the walk (a merge, a retired
/// index) is skipped.
fn dir_bytes(root: &Path, max_entries: usize) -> io::Result<u64> {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            seen += 1;
            if seen > max_entries {
                return Err(io::Error::other("index walk passed its entry cap"));
            }
            let entry = entry?;
            // `DirEntry::metadata` doesn't follow symlinks.
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if meta.is_dir() {
                dirs.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

/// Samples now and every `interval` until `stop` turns true (or its
/// sender goes). A sample under way when it does is left to finish on its
/// blocking thread: shutdown never waits on a slow disk.
pub fn spawn(
    paths: Paths,
    gauges: Arc<Gauges>,
    interval: Duration,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.wait_for(|s| *s) => return,
            }
            let (p, g) = (paths.clone(), gauges.clone());
            let work = tokio::task::spawn_blocking(move || sample(&p, &g, MAX_ENTRIES));
            tokio::select! {
                _ = work => {}
                _ = stop.wait_for(|s| *s) => return,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn write(path: &Path, len: usize) {
        std::fs::write(path, vec![0u8; len]).unwrap();
    }

    fn fixture() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            db: dir.path().join("lodestar.db"),
            index: dir.path().join("index"),
        };
        write(&paths.db, 4096);
        let v = paths.index.join("v1-t1");
        std::fs::create_dir_all(v.join("files")).unwrap();
        write(&v.join("a.idx"), 1000);
        write(&v.join("meta.json"), 24);
        write(&v.join("files").join("b.idx"), 500);
        (dir, paths)
    }

    #[test]
    fn values_match_files_of_known_size() {
        let (_dir, paths) = fixture();
        write(&wal_path(&paths.db), 777);
        let g = Gauges::default();
        sample(&paths, &g, MAX_ENTRIES);
        assert_eq!(g.db_bytes.load(Ordering::Relaxed), 4096);
        assert_eq!(g.wal_bytes.load(Ordering::Relaxed), 777);
        assert_eq!(g.index_bytes.load(Ordering::Relaxed), 1524);
        assert_eq!(g.errors.load(Ordering::Relaxed), 0);
        assert!(g.sampled_at.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn absent_wal_and_index_are_zero() {
        let (_dir, paths) = fixture();
        std::fs::remove_dir_all(&paths.index).unwrap();
        let g = Gauges::default();
        g.wal_bytes.store(9, Ordering::Relaxed);
        sample(&paths, &g, MAX_ENTRIES);
        assert_eq!(g.wal_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(g.index_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(g.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_failure_keeps_the_last_value_and_counts() {
        let (_dir, paths) = fixture();
        let g = Gauges::default();
        sample(&paths, &g, MAX_ENTRIES);
        let at = g.sampled_at.load(Ordering::Relaxed);
        // The database gone, and an index walk past its cap.
        std::fs::remove_file(&paths.db).unwrap();
        g.sampled_at.store(1, Ordering::Relaxed);
        sample(&paths, &g, 2);
        assert_eq!(g.db_bytes.load(Ordering::Relaxed), 4096);
        assert_eq!(g.index_bytes.load(Ordering::Relaxed), 1524);
        assert_eq!(g.errors.load(Ordering::Relaxed), 2);
        assert_eq!(g.sampled_at.load(Ordering::Relaxed), 1, "was {at}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let (dir, paths) = fixture();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        write(&outside.join("big"), 1 << 20);
        let v = paths.index.join("v1-t1");
        std::os::unix::fs::symlink(&outside, v.join("dir-link")).unwrap();
        std::os::unix::fs::symlink(outside.join("big"), v.join("file-link")).unwrap();
        assert_eq!(dir_bytes(&paths.index, MAX_ENTRIES).unwrap(), 1524);
    }

    async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn the_task_samples_and_stops_promptly() {
        let (_dir, paths) = fixture();
        let g = Arc::new(Gauges::default());
        let (stop, stopped) = watch::channel(false);
        // A long interval: the first sample is immediate, and stopping
        // mustn't wait for the next tick.
        let task = spawn(paths, g.clone(), Duration::from_secs(3600), stopped);
        eventually("the first sample", || {
            g.sampled_at.load(Ordering::Acquire) > 0
        })
        .await;
        assert_eq!(g.db_bytes.load(Ordering::Relaxed), 4096);
        let _ = stop.send(true);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the sampler stops without waiting for its interval")
            .unwrap();
    }
}
