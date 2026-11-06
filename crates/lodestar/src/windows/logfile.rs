//! The service's log file. A Windows service has no console, so its log
//! (with the first run's setup token, §12) goes to
//! `<data>/logs/lodestar.log`. The file is capped: past [`MAX_BYTES`] it
//! becomes `lodestar.log.1` (replacing the previous one) and a new file
//! starts, so the folder normally holds no more than twice the cap.
//!
//! While another program holds the file without sharing delete (a log
//! viewer, say), Windows refuses the rename. Records then go on into the
//! current file, and the rename is tried again only after
//! [`ROTATION_RETRY`], as the JSONL file sink does. The current file
//! stops at twice the cap ([`LogFile`] has the bound), so the folder holds
//! at most three times the cap.
//!
//! Since B-log1b the service's logging runtime writes its console here
//! ([`Console`]), beside the structured `lodestar.jsonl`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;

/// One file's cap (§12.1's default rotation size).
pub const MAX_BYTES: u64 = 16 << 20;

/// How long after a failed rotation the next one is tried: the value and
/// meaning of the JSONL file sink's (`logging::file`).
pub const ROTATION_RETRY: Duration = Duration::from_secs(60);

/// `<data>/logs/lodestar.log`.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("lodestar.log")
}

/// Creates the logs folder if it's missing. A folder made here holds the
/// setup token, and is made before the runtime's file sink would make it
/// (and lock it down), so it's locked down here the same way: only its
/// owner, SYSTEM and Administrators, whatever an admin-chosen data folder
/// lets Users read. Failing that, it's removed again and nothing is
/// written. An existing folder keeps its ACL; the file sink checks it.
fn create_private(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e),
    }
    if let Err(e) = lodestar_integrations::secret::restrict_dir_acl(dir) {
        let _ = fs::remove_dir(dir);
        return Err(e);
    }
    // Until the ACL changed, whoever may write to the data folder could
    // plant an entry in the new folder (it keeps its own ACL) or swap it
    // for a link: it must still be an empty real folder.
    let real = fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir());
    let empty = fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none());
    let planted = !(real && empty);
    if planted {
        let _ = fs::remove_dir(dir);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the new logs folder changed before its access was restricted",
        ));
    }
    Ok(())
}

/// An append-only log file that rotates once at `max` bytes.
///
/// **Bound while a rotation waits.** A failed rename (another program
/// holds the file) isn't tried again for [`ROTATION_RETRY`]; records keep
/// landing whole and in order in the current file meanwhile. So the file
/// reaches the cap plus what [`ROTATION_RETRY`] of writes adds, and grows
/// by that again for each further failed attempt, but never past twice
/// the cap: a record that would take a non-empty file past that is
/// refused (an error, as the JSONL sink refuses lines at its total cap)
/// until a rotation succeeds. The folder then holds at most three times
/// the cap.
#[derive(Debug)]
pub struct LogFile {
    path: PathBuf,
    max: u64,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// `None` after a failed reopen; the next write tries again.
    file: Option<File>,
    size: u64,
    /// No rotation before this, after one failed.
    retry_at: Option<Instant>,
    /// Rename attempts so far.
    #[cfg(test)]
    renames: u32,
}

impl LogFile {
    /// Opens (or creates) `path` for appending, creating its folder.
    pub fn open(path: PathBuf, max: u64) -> io::Result<LogFile> {
        if let Some(dir) = path.parent() {
            create_private(dir)?;
        }
        let file = append(&path)?;
        let size = file.metadata()?.len();
        Ok(LogFile {
            path,
            max,
            inner: Mutex::new(Inner {
                file: Some(file),
                size,
                retry_at: None,
                #[cfg(test)]
                renames: 0,
            }),
        })
    }

    /// The previous file, `lodestar.log.1`.
    pub fn rotated_path(&self) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(".1");
        PathBuf::from(name)
    }

    /// Writes one record whole, rotating first if it would pass the cap.
    fn write_record(&self, buf: &[u8]) -> io::Result<()> {
        self.write_record_at(buf, Instant::now())
    }

    /// [`write_record`](Self::write_record) at `now`, the tests' clock.
    fn write_record_at(&self, buf: &[u8], now: Instant) -> io::Result<()> {
        let mut inner = self.lock();
        let len = buf.len() as u64;
        let over = inner.size > 0 && inner.size + len > self.max;
        // As in the JSONL sink, only a successful rename ends the wait.
        if over && inner.retry_at.is_none_or(|t| now >= t) {
            // Closed before the rename: Windows won't rename an open file
            // unless every handle allows it.
            inner.file = None;
            #[cfg(test)]
            {
                inner.renames += 1;
            }
            match fs::rename(&self.path, self.rotated_path()) {
                Ok(()) => {
                    inner.size = 0;
                    inner.retry_at = None;
                }
                // Another program has the file open: keep appending
                // rather than lose records, and don't pay for a close,
                // a rename and a reopen on every record meanwhile.
                Err(_) => inner.retry_at = Some(now + ROTATION_RETRY),
            }
        }
        if inner.file.is_none() {
            let file = append(&self.path)?;
            inner.size = file.metadata()?.len();
            inner.file = Some(file);
        }
        if inner.size > 0 && inner.size + len > self.ceiling() {
            return Err(io::Error::other(
                "the log file is at twice its cap and can't rotate yet",
            ));
        }
        let file = inner.file.as_mut().expect("opened above");
        file.write_all(buf)?;
        inner.size += len;
        Ok(())
    }

    /// Where a file that can't rotate stops: twice the cap.
    fn ceiling(&self) -> u64 {
        self.max.saturating_mul(2)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic mid-write leaves nothing to repair: keep logging.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// One record's writer: `tracing`'s formatter writes each record in one
/// call, so a record never straddles two files.
#[derive(Debug)]
pub struct Record<'a>(&'a LogFile);

impl Write for Record<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_record(buf).map(|()| buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = Record<'a>;

    fn make_writer(&'a self) -> Record<'a> {
        Record(self)
    }
}

/// The logging runtime's console for the service: what it writes for a
/// line is kept until it flushes, then written as one record, so a line
/// never straddles a rotation.
#[derive(Debug)]
pub struct Console {
    file: Arc<LogFile>,
    pending: Vec<u8>,
}

impl Console {
    pub fn new(file: Arc<LogFile>) -> Console {
        Console {
            file,
            pending: Vec::new(),
        }
    }
}

impl Write for Console {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let line = std::mem::take(&mut self.pending);
        self.file.write_record(&line)
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(log: &LogFile, s: &str) {
        log.make_writer().write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn appends_across_opens() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        assert!(p.ends_with("logs/lodestar.log"));
        write(&LogFile::open(p.clone(), 100).unwrap(), "one\n");
        write(&LogFile::open(p.clone(), 100).unwrap(), "two\n");
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\ntwo\n");
    }

    #[test]
    fn rotates_whole_records_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let log = LogFile::open(p.clone(), 10).unwrap();
        write(&log, "aaaa\n");
        write(&log, "bbbb\n"); // exactly 10: stays
        write(&log, "cccc\n"); // would pass 10: rotates first
        assert_eq!(
            fs::read_to_string(log.rotated_path()).unwrap(),
            "aaaa\nbbbb\n"
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), "cccc\n");
        write(&log, "dddddddddddddddd\n"); // longer than the cap alone
        write(&log, "e\n");
        // Only one old file is kept.
        assert_eq!(
            fs::read_to_string(log.rotated_path()).unwrap(),
            "dddddddddddddddd\n"
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), "e\n");
    }

    #[test]
    fn a_reopened_file_counts_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        write(&LogFile::open(p.clone(), 10).unwrap(), "12345678\n");
        let log = LogFile::open(p.clone(), 10).unwrap();
        write(&log, "next\n");
        assert_eq!(
            fs::read_to_string(log.rotated_path()).unwrap(),
            "12345678\n"
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), "next\n");
    }

    #[test]
    fn recovers_when_the_file_disappears() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let log = LogFile::open(p.clone(), 10).unwrap();
        write(&log, "aaaaaaaa\n");
        fs::remove_dir_all(dir.path().join("logs")).unwrap();
        // Neither the rename nor the reopen works: this record is lost,
        // not the next.
        assert!(log.make_writer().write_all(b"bbbb\n").is_err());
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        write(&log, "cccc\n");
        assert_eq!(fs::read_to_string(&p).unwrap(), "cccc\n");
    }

    fn write_at(log: &LogFile, s: &str, now: Instant) {
        log.write_record_at(s.as_bytes(), now).unwrap();
    }

    fn renames(log: &LogFile) -> u32 {
        log.lock().renames
    }

    /// Makes every rename fail on any platform: the rotated name is a
    /// folder that isn't empty. Returns what lifts it.
    fn block_rotation(log: &LogFile) -> impl FnOnce() {
        let blocker = log.rotated_path();
        fs::create_dir(&blocker).unwrap();
        fs::write(blocker.join("held"), b"").unwrap();
        move || fs::remove_dir_all(&blocker).unwrap()
    }

    #[test]
    fn a_failed_rotation_waits_before_trying_again() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let log = LogFile::open(p.clone(), 10).unwrap();
        let t0 = Instant::now();
        write_at(&log, "aaaa\n", t0);
        write_at(&log, "bbbb\n", t0);
        let lift = block_rotation(&log);
        write_at(&log, "cccc\n", t0); // past the cap: the rename fails
        assert_eq!(renames(&log), 1);
        write_at(&log, "d\n", t0 + Duration::from_secs(1));
        lift();
        // Free again, but the delay hasn't passed: no attempt.
        write_at(&log, "e\n", t0 + ROTATION_RETRY - Duration::from_millis(1));
        assert_eq!(renames(&log), 1);
        assert!(!log.rotated_path().exists());
        assert_eq!(fs::read_to_string(&p).unwrap(), "aaaa\nbbbb\ncccc\nd\ne\n");
        // Once it has, the next record rotates first.
        write_at(&log, "ffff\n", t0 + ROTATION_RETRY);
        assert_eq!(renames(&log), 2);
        assert_eq!(
            fs::read_to_string(log.rotated_path()).unwrap(),
            "aaaa\nbbbb\ncccc\nd\ne\n"
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), "ffff\n");
        // The wait is over: the next cap rotates at once.
        write_at(&log, "gggg\n", t0 + ROTATION_RETRY);
        write_at(&log, "hhhh\n", t0 + ROTATION_RETRY);
        assert_eq!(renames(&log), 3);
        assert_eq!(fs::read_to_string(&p).unwrap(), "hhhh\n");
    }

    #[test]
    fn each_failed_attempt_starts_a_new_wait() {
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile::open(path(dir.path()), 10).unwrap();
        let t0 = Instant::now();
        write_at(&log, "aaaaaaaa\n", t0);
        let lift = block_rotation(&log);
        write_at(&log, "b\n", t0);
        let t1 = t0 + ROTATION_RETRY;
        write_at(&log, "c\n", t1); // tried again, failed again
        assert_eq!(renames(&log), 2);
        write_at(&log, "d\n", t1 + ROTATION_RETRY - Duration::from_millis(1));
        assert_eq!(renames(&log), 2);
        lift();
        write_at(&log, "e\n", t1 + ROTATION_RETRY);
        assert_eq!(renames(&log), 3);
        assert_eq!(
            fs::read_to_string(log.rotated_path()).unwrap(),
            "aaaaaaaa\nb\nc\nd\n"
        );
    }

    /// The current file never passes twice the cap; records refused at
    /// that ceiling are the only ones lost, and only while it can't
    /// rotate.
    #[test]
    fn a_file_that_cant_rotate_stops_at_twice_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let log = LogFile::open(p.clone(), 10).unwrap();
        let t0 = Instant::now();
        let lift = block_rotation(&log);
        for line in ["aaaa\n", "bbbb\n", "cccc\n", "dddd\n"] {
            write_at(&log, line, t0);
        }
        assert_eq!(fs::metadata(&p).unwrap().len(), 20);
        assert!(log.write_record_at(b"e\n", t0).is_err());
        assert_eq!(fs::metadata(&p).unwrap().len(), 20);
        lift();
        write_at(&log, "ffff\n", t0 + ROTATION_RETRY);
        assert_eq!(
            fs::read_to_string(log.rotated_path()).unwrap(),
            "aaaa\nbbbb\ncccc\ndddd\n"
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), "ffff\n");
    }

    /// A reader that holds `lodestar.log` without sharing delete, as a
    /// log viewer may: the rename fails, every record still lands, and
    /// the rename isn't tried again until the delay has passed. Once the
    /// reader lets go and the delay is over, the file rotates.
    #[cfg(windows)]
    #[test]
    fn a_reader_holding_the_file_delays_rotation() {
        use std::io::Read;
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;

        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let log = LogFile::open(p.clone(), 20).unwrap();
        let t0 = Instant::now();
        for line in ["aaaa\n", "bbbb\n", "cccc\n", "dddd\n"] {
            write_at(&log, line, t0);
        }
        let mut reader = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&p)
            .unwrap();
        write_at(&log, "eeee\n", t0); // past the cap: refused by Windows
        assert_eq!(renames(&log), 1);
        assert!(!log.rotated_path().exists());
        write_at(&log, "ffff\n", t0 + Duration::from_secs(30));
        write_at(
            &log,
            "gggg\n",
            t0 + ROTATION_RETRY - Duration::from_millis(1),
        );
        assert_eq!(renames(&log), 1);
        let all = "aaaa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\n";
        let mut seen = String::new();
        reader.read_to_string(&mut seen).unwrap();
        assert_eq!(seen, all);
        drop(reader);
        write_at(&log, "hhhh\n", t0 + ROTATION_RETRY);
        assert_eq!(renames(&log), 2);
        assert_eq!(fs::read_to_string(log.rotated_path()).unwrap(), all);
        assert_eq!(fs::read_to_string(&p).unwrap(), "hhhh\n");
    }

    /// A logs folder made here is private even inside a data folder that
    /// lets Users read; an existing one keeps its ACL.
    #[cfg(windows)]
    #[test]
    fn a_new_logs_folder_is_locked_down_first() {
        let dir = tempfile::tempdir().unwrap();
        // A data folder anyone in Users may read, as an admin might pick.
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let icacls = Path::new(&root).join("System32").join("icacls.exe");
        let shared = |p: &Path| {
            let ok = std::process::Command::new(&icacls)
                .arg(p)
                .args(["/grant", "*S-1-5-32-545:(OI)(CI)R"])
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok);
        };
        shared(dir.path());
        let _log = LogFile::open(path(dir.path()), MAX_BYTES).unwrap();
        crate::windows::acl::log_folder(&dir.path().join("logs")).unwrap();
        // A folder that was already there isn't touched.
        let kept = dir.path().join("kept");
        fs::create_dir(&kept).unwrap();
        let _log = LogFile::open(kept.join("lodestar.log"), MAX_BYTES).unwrap();
        assert!(crate::windows::acl::log_folder(&kept).is_err());
    }

    #[test]
    fn the_console_writes_each_line_whole() {
        let dir = tempfile::tempdir().unwrap();
        let file = Arc::new(LogFile::open(path(dir.path()), 40).unwrap());
        let mut c = Console::new(file.clone());
        for n in 0..3 {
            c.write_all(format!("line number {n} of three").as_bytes())
                .unwrap();
            c.write_all(b"\n").unwrap();
            c.flush().unwrap();
        }
        // Each line went whole into one file or the other.
        let now = fs::read_to_string(path(dir.path())).unwrap();
        let old = fs::read_to_string(file.rotated_path()).unwrap();
        assert_eq!(now, "line number 2 of three\n");
        assert_eq!(old, "line number 1 of three\n");
    }

    /// The service's runtime writes its console here: the setup token's
    /// line reads as it does on a console, token right after the words,
    /// and its JSONL file goes to the logs folder beside it.
    #[test]
    fn the_service_runtime_writes_the_token_line_here() {
        use crate::logging::Runtime;
        use crate::logging::state::Applied;
        use crate::settings::logging::Logging;
        use lodestar_log::{Record, Sink, Value};

        let dir = tempfile::tempdir().unwrap();
        let file = Arc::new(LogFile::open(path(dir.path()), MAX_BYTES).unwrap());
        let section = Logging {
            file_folder: dir.path().join("logs").display().to_string(),
            ..Logging::default()
        };
        let rt = Runtime::new(
            Box::new(Console::new(file)),
            Applied::compute(&section, crate::unix_now()),
        );
        rt.open_file();
        let fields = [
            ("token", Value::Secret("ABCD-1234-EFGH-5678")),
            ("address", Value::Listen("0.0.0.0:8191".parse().unwrap())),
        ];
        rt.record(&Record {
            event: &crate::logging::events::AUTH_SETUP_TOKEN::EVENT,
            fields: &fields,
        });
        let log = fs::read_to_string(path(dir.path())).unwrap();
        assert!(
            log.contains(
                "enter setup token ABCD-1234-EFGH-5678 address=\"http://<this host's address>:8191/\"\n"
            ),
            "{log}"
        );
        let jsonl = fs::read_to_string(dir.path().join("logs").join("lodestar.jsonl")).unwrap();
        assert!(jsonl.contains(r#""event":"auth.setup_token""#), "{jsonl}");
    }
}
