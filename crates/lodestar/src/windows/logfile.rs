//! The service's log file. A Windows service has no console, so its log
//! (with the first run's setup token, §12) goes to
//! `<data>/logs/lodestar.log`. The file is capped: past [`MAX_BYTES`] it
//! becomes `lodestar.log.1` (replacing the previous one) and a new file
//! starts, so the folder never holds more than twice the cap.
//!
//! This stands in until the structured log file of §12.1 exists.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tracing_subscriber::fmt::MakeWriter;

/// One file's cap (§12.1's default rotation size).
pub const MAX_BYTES: u64 = 16 << 20;

/// `<data>/logs/lodestar.log`.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("lodestar.log")
}

/// An append-only log file that rotates once at `max` bytes.
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
}

impl LogFile {
    /// Opens (or creates) `path` for appending, creating its folder.
    pub fn open(path: PathBuf, max: u64) -> io::Result<LogFile> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = append(&path)?;
        let size = file.metadata()?.len();
        Ok(LogFile {
            path,
            max,
            inner: Mutex::new(Inner {
                file: Some(file),
                size,
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
        let mut inner = self.lock();
        if inner.size > 0 && inner.size + buf.len() as u64 > self.max {
            // Closed before the rename: Windows won't rename an open file
            // unless every handle allows it.
            inner.file = None;
            // If the rename fails (another program has the file open),
            // keep appending rather than lose records; the next record
            // tries again.
            let _ = fs::rename(&self.path, self.rotated_path());
            inner.size = 0;
        }
        if inner.file.is_none() {
            let file = append(&self.path)?;
            inner.size = file.metadata()?.len();
            inner.file = Some(file);
        }
        let file = inner.file.as_mut().expect("opened above");
        file.write_all(buf)?;
        inner.size += buf.len() as u64;
        Ok(())
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
}
