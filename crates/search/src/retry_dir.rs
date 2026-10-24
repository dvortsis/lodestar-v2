//! The index directory, with a Windows sharing conflict on an atomic
//! write retried.
//!
//! tantivy writes `meta.json` (and the other managed files) by writing a
//! temporary file and renaming it over the old one: `tempfile`'s
//! `persist`, which on Windows is `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`.
//! Windows refuses to replace a file another handle has open, even one
//! opened with `FILE_SHARE_DELETE` (std's default), with
//! `ERROR_ACCESS_DENIED` (os error 5), or `ERROR_SHARING_VIOLATION` (32)
//! when the handle doesn't share delete. Reading `meta.json` while a
//! commit replaces it is ordinary here: the percolator, `/saved` and
//! `/filters` read the watermark (`load_metas`) while the live indexer
//! commits. A reader holds the file for a few milliseconds, so the
//! rename succeeds moments later.
//!
//! Without the retry the commit fails, and the indexer reopens its writer
//! and reads the store again after a backoff: it recovers, but counts an
//! error and logs a warning each time, and on a busy index it can lose
//! seconds of freshness.
//!
//! On other systems the rename never conflicts, and a permission error is
//! real: nothing is retried there.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    Directory, DirectoryLock, FileHandle, FileSlice, Lock, WatchCallback, WatchHandle, WritePtr,
};

/// The attempts an atomic write makes, and the first pause between two;
/// each pause doubles, up to [`MAX_PAUSE`]: about 1.3 s in all, far more
/// than a reader holds `meta.json`.
const ATTEMPTS: u32 = 8;
const FIRST_PAUSE: Duration = Duration::from_millis(5);
const MAX_PAUSE: Duration = Duration::from_millis(320);

/// Wraps a directory; every method but `atomic_write` passes through
/// unchanged, `acquire_lock` included (`MmapDirectory`'s OS file locks).
#[derive(Clone, Debug)]
pub(crate) struct Retrying<D> {
    inner: D,
    transient: fn(&io::Error) -> bool,
}

impl<D> Retrying<D> {
    pub(crate) fn new(inner: D) -> Retrying<D> {
        Retrying {
            inner,
            transient: sharing_conflict,
        }
    }
}

/// A rename over a file another handle holds (Windows only).
fn sharing_conflict(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32))
}

/// Runs `op` until it succeeds, fails with an error `transient` doesn't
/// accept, or has made [`ATTEMPTS`] attempts (the last error is returned).
fn retry(
    transient: fn(&io::Error) -> bool,
    mut op: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut pause = FIRST_PAUSE;
    let mut attempt = 1;
    loop {
        match op() {
            Err(e) if attempt < ATTEMPTS && transient(&e) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(MAX_PAUSE);
                attempt += 1;
            }
            result => return result,
        }
    }
}

impl<D: Directory + Clone> Directory for Retrying<D> {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.inner.get_file_handle(path)
    }

    fn open_read(&self, path: &Path) -> Result<FileSlice, OpenReadError> {
        self.inner.open_read(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.inner.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.inner.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        self.inner.open_write(path)
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.inner.atomic_read(path)
    }

    /// Each attempt writes a fresh temporary file and renames it, so a
    /// retry never sees a half-written one.
    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        retry(self.transient, || self.inner.atomic_write(path, data))
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.inner.sync_directory()
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.inner.acquire_lock(lock)
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.inner.watch(watch_callback)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tantivy::directory::RamDirectory;

    use super::*;

    fn denied() -> io::Error {
        io::Error::from_raw_os_error(5)
    }

    fn any_os_5_or_32(e: &io::Error) -> bool {
        matches!(e.raw_os_error(), Some(5 | 32))
    }

    #[test]
    fn a_conflict_is_retried_until_the_write_succeeds() {
        let calls = Cell::new(0);
        let result = retry(any_os_5_or_32, || {
            calls.set(calls.get() + 1);
            if calls.get() < 4 {
                Err(denied())
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok());
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn a_conflict_that_lasts_gives_up_with_the_error() {
        let calls = Cell::new(0);
        let result = retry(any_os_5_or_32, || {
            calls.set(calls.get() + 1);
            Err(denied())
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
        assert_eq!(calls.get(), ATTEMPTS);
    }

    #[test]
    fn any_other_error_fails_at_once() {
        let calls = Cell::new(0);
        let result = retry(any_os_5_or_32, || {
            calls.set(calls.get() + 1);
            Err(io::Error::other("disk full"))
        });
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    /// Off Windows a permission error is real: never retried.
    #[test]
    #[cfg(not(windows))]
    fn elsewhere_nothing_is_a_sharing_conflict() {
        assert!(!sharing_conflict(&denied()));
        assert!(!sharing_conflict(&io::Error::from_raw_os_error(32)));
    }

    #[test]
    #[cfg(windows)]
    fn on_windows_access_denied_and_sharing_violations_are() {
        assert!(sharing_conflict(&denied()));
        assert!(sharing_conflict(&io::Error::from_raw_os_error(32)));
        assert!(!sharing_conflict(&io::Error::from_raw_os_error(2)));
    }

    /// The wrapper is a whole directory: an index commits and reads
    /// through it.
    #[test]
    fn an_index_commits_through_it() {
        use tantivy::schema::{STORED, Schema, TEXT};
        use tantivy::{Index, doc};

        let mut schema = Schema::builder();
        let title = schema.add_text_field("title", TEXT | STORED);
        let index = Index::create(
            Retrying::new(RamDirectory::create()),
            schema.build(),
            Default::default(),
        )
        .unwrap();
        let mut writer = index.writer_with_num_threads(1, 15_000_000).unwrap();
        writer.add_document(doc!(title => "one")).unwrap();
        writer.commit().unwrap();
        let reader = index.reader().unwrap();
        assert_eq!(reader.searcher().num_docs(), 1);
        assert!(index.load_metas().is_ok());
    }
}
