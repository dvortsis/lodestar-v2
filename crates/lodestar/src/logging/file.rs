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

/// `index.json`: the files with a flag set, by name.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub files: BTreeMap<String, Flags>,
    #[serde(skip)]
    path: PathBuf,
}

impl Index {
    fn load(dir: &Path) -> Index {
        let path = dir.join(INDEX);
        let mut index: Index = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        index.path = path;
        index
    }

    fn save(&self) -> io::Result<()> {
        let tmp = self.path.with_extension("json.part");
        fs::write(&tmp, serde_json::to_vec(self).expect("an index serializes"))?;
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

    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        match self.files.remove(from) {
            Some(f) => {
                self.files.insert(to.to_string(), f);
                self.save()
            }
            None => Ok(()),
        }
    }

    fn remove(&mut self, name: &str) -> io::Result<()> {
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

enum Job {
    Compress(String, FileConfig),
    Prune(FileConfig),
    #[cfg(test)]
    Barrier(mpsc::Sender<()>),
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
                        Job::Compress(name, cfg) => compress(&cfg.dir, &name, &index)
                            .and_then(|()| prune(&cfg, &index, SystemTime::now())),
                        Job::Prune(cfg) => prune(&cfg, &index, SystemTime::now()),
                        #[cfg(test)]
                        Job::Barrier(done) => {
                            let _ = done.send(());
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
    #[cfg(test)]
    pub fail_rotation: bool,
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl FileSink {
    /// Opens (or creates) the current file in `cfg.dir`, compresses
    /// rotated files a previous run left uncompressed, and prunes.
    pub fn open(cfg: FileConfig) -> io::Result<FileSink> {
        fs::create_dir_all(&cfg.dir)?;
        let path = cfg.dir.join(CURRENT);
        let index = Arc::new(Mutex::new(Index::load(&cfg.dir)));
        let worker = Worker::start(index.clone());
        let file = open_append(&path)?;
        let meta = file.metadata()?;
        let flags = lock(&index).files.get(CURRENT).copied().unwrap_or_default();
        let mut sink = FileSink {
            path,
            size: meta.len(),
            opened: SystemTime::now(),
            file,
            index,
            flags,
            worker,
            retry_at: None,
            cfg,
            #[cfg(test)]
            fail_rotation: false,
        };
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
    /// one, and hands the old one to the background thread.
    pub fn rotate(&mut self) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_rotation {
            return Err(io::Error::other("rotation refused by the test"));
        }
        let name = self.rotated_name();
        let to = self.cfg.dir.join(&name);
        fs::rename(&self.path, &to)?;
        // On a failure the renamed file stays open: lines go on there, and
        // the next rotation tries again.
        self.file = open_append(&self.path)?;
        self.size = 0;
        self.opened = SystemTime::now();
        self.flags = Flags::default();
        let _ = lock(&self.index).rename(CURRENT, &name);
        self.retry_at = None;
        self.worker.send(Job::Compress(name, self.cfg.clone()));
        Ok(())
    }

    fn rotated_name(&self) -> String {
        let stamp = Stamp::now().compact();
        let mut n = 1;
        loop {
            let name = match n {
                1 => format!("lodestar-{stamp}.jsonl"),
                n => format!("lodestar-{stamp}-{n}.jsonl"),
            };
            let gz = format!("{name}.gz");
            if !self.cfg.dir.join(&name).exists() && !self.cfg.dir.join(gz).exists() {
                return name;
            }
            n += 1;
        }
    }

    /// Waits for the background thread to finish what it was given.
    #[cfg(test)]
    pub fn settle(&self) {
        let (tx, rx) = mpsc::channel();
        self.worker.send(Job::Barrier(tx));
        let _ = rx.recv_timeout(Duration::from_secs(30));
    }
}

fn lock(index: &Mutex<Index>) -> std::sync::MutexGuard<'_, Index> {
    index.lock().unwrap_or_else(|e| e.into_inner())
}

/// The rotated files in `dir`, newest first (their names hold the time).
fn rotated(dir: &Path) -> io::Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            n.starts_with("lodestar-") && (n.ends_with(".jsonl") || n.ends_with(".jsonl.gz"))
        })
        .collect();
    names.sort_by(|a, b| b.cmp(a));
    Ok(names)
}

fn compress(dir: &Path, name: &str, index: &Mutex<Index>) -> io::Result<()> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let from = dir.join(name);
    let gz = format!("{name}.gz");
    let part = dir.join(format!("{gz}.part"));
    let mut input = File::open(&from)?;
    let mut out = GzEncoder::new(File::create(&part)?, Compression::default());
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
    }
    out.finish()?.sync_all()?;
    fs::rename(&part, dir.join(&gz))?;
    fs::remove_file(&from)?;
    let _ = lock(index).rename(name, &gz);
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

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_full_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/dev/full", dir.path().join(CURRENT)).unwrap();
        let mut sink = FileSink::open(cfg(dir.path(), 1 << 20)).unwrap();
        assert!(matches!(
            sink.write("x", Flags::default()),
            Err(WriteError::DiskFull)
        ));
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
}
