//! Helpers the maintenance commands share: outcomes, the update-rollback
//! guard, durable file operations, sizes and times.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lodestar_store::rusqlite::{self, Connection, OpenFlags, OptionalExtension};

/// Why a command didn't do its job.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// It refused for safety (the store is in use, an update rollback is
    /// in progress, no confirmation, …). Nothing was changed.
    Refused(String),
    /// Something went wrong.
    Failed(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Refused(m) => write!(f, "refused: {m}"),
            Failure::Failed(m) => f.write_str(m),
        }
    }
}

pub type Outcome<T = ()> = Result<T, Failure>;

/// `Failed` with the context `what` in front of the error.
pub fn failed<E: fmt::Display>(what: impl fmt::Display) -> impl FnOnce(E) -> Failure {
    move |e| Failure::Failed(format!("{what}: {e}"))
}

/// B-update's journal (architecture §12.2): while it exists no process
/// may open the store, because the supervisor is restoring it.
pub fn rollback_journal(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("rollback.json")
}

/// Refuses while an update rollback is in progress.
pub fn refuse_during_update_rollback(data_dir: &Path) -> Outcome {
    let journal = rollback_journal(data_dir);
    if journal.exists() {
        return Err(Failure::Refused(format!(
            "an update rollback is in progress ({}). Start Lodestar to let it finish; \
             nothing else may open the store until then.",
            journal.display()
        )));
    }
    Ok(())
}

/// A read-only connection that never creates the file.
pub fn open_read_only(db: &Path) -> Outcome<Connection> {
    if !db.is_file() {
        return Err(Failure::Failed(format!("no store at {}", db.display())));
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(failed(format!("opening {}", db.display())))?;
    conn.busy_timeout(std::time::Duration::from_secs(30))
        .map_err(failed("setting the busy timeout"))?;
    Ok(conn)
}

/// The highest applied migration (0 for an empty or foreign database).
pub fn schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations')",
        [],
        |r| r.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    Ok(conn
        .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten()
        .unwrap_or(0))
}

/// Torrents stored (a full count; fine for a command, not a hot path).
pub fn torrent_count(conn: &Connection) -> rusqlite::Result<Option<u64>> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'torrents')",
        [],
        |r| r.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    conn.query_row("SELECT count(*) FROM torrents", [], |r| r.get::<_, i64>(0))
        .map(|n| Some(n.max(0) as u64))
}

/// SQLite's `quick_check`: `Ok` when the file is a sound database.
pub fn quick_check(conn: &Connection) -> Result<(), String> {
    let first: String = conn
        .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if first == "ok" { Ok(()) } else { Err(first) }
}

/// `VACUUM INTO` a new file, then fsyncs it: a consistent, compact copy
/// of `conn`'s database, taken inside one read transaction.
pub fn vacuum_into(conn: &Connection, to: &Path) -> Outcome {
    if to.exists() {
        return Err(Failure::Failed(format!("{} exists already", to.display())));
    }
    let target = to
        .to_str()
        .ok_or_else(|| Failure::Failed(format!("{} isn't valid UTF-8", to.display())))?;
    let written = conn
        .execute("VACUUM INTO ?1", [target])
        .map_err(failed(format!("writing {}", to.display())))
        .and_then(|_| sync_file(to).map_err(failed(format!("syncing {}", to.display()))));
    if written.is_err() {
        // It didn't exist before: a partial copy is ours to remove.
        let _ = fs::remove_file(to);
    }
    written
}

/// Flushes a file to disk. Windows' `FlushFileBuffers` needs write access,
/// so it's opened for writing (never truncated).
pub fn sync_file(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()
}

/// Says which account a permission error happened as: the commands run
/// as PUID/PGID once root is dropped, so a folder root can write may not
/// be writable.
pub fn explain_denied(e: &io::Error) -> String {
    if e.kind() != io::ErrorKind::PermissionDenied {
        return e.to_string();
    }
    #[cfg(unix)]
    {
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        format!(
            "{e}. Lodestar runs as {uid}:{gid} (PUID/PGID), so the folder must be writable by \
             that account"
        )
    }
    #[cfg(not(unix))]
    {
        format!("{e}. The folder must be writable by the account Lodestar runs as")
    }
}

/// Makes a rename or a new file in `dir` durable. Windows can't open a
/// directory for this; NTFS journals the rename itself.
pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Creates `path` exclusively (never overwriting), readable by the owner
/// only on Unix since backups hold secrets.
pub fn create_new_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Copies `from` to a new file `to` (never overwriting) and fsyncs it.
pub fn copy_new_private(from: &Path, to: &Path) -> io::Result<u64> {
    let mut src = File::open(from)?;
    let mut dst = create_new_private(to)?;
    let n = io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    Ok(n)
}

/// Removes a file if it exists.
pub fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// `<db>-wal`, `<db>-shm` and `<db>-journal`.
pub fn side_files(db: &Path) -> [PathBuf; 3] {
    let with = |suffix: &str| {
        let mut name = db.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    };
    [with("-wal"), with("-shm"), with("-journal")]
}

/// The store's size on disk: the file plus its WAL.
pub fn store_bytes(db: &Path) -> u64 {
    let [wal, ..] = side_files(db);
    file_len(db) + file_len(&wal)
}

pub fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

/// Bytes under `dir`, not following symlinks.
pub fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// IEC sizes with the digits of `docs/metrics.md`'s `formatBytes`
/// (`512 B`, `4.37 GiB`, `38.2 MiB`, `912 KiB`), with a plain space.
pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut unit = 0;
    let mut value = n as f64;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{n} B");
    }
    let digits = |v: f64| match v {
        v if v < 10.0 => 2,
        v if v < 100.0 => 1,
        _ => 0,
    };
    let mut d = digits(value);
    let mut text = format!("{value:.d$}");
    // Rounding up to 1024 steps up a unit ("1.00 MiB", never "1024 KiB").
    if text.parse::<f64>().is_ok_and(|v| v >= 1024.0) && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
        d = digits(value);
        text = format!("{value:.d$}");
    }
    format!("{text} {}", UNITS[unit])
}

/// Thousands separated by commas: `1,234,567`.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// When a file was last modified, as Unix seconds.
pub fn modified_secs(path: &Path) -> Option<i64> {
    let t = fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// `(year, month, day, hour, minute, second)` in UTC.
fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        month,
        day,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

/// `2026-10-22 18:04 UTC`, for people.
pub fn format_time(secs: i64) -> String {
    let (y, mo, d, h, mi, _) = civil(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
}

/// `20261022-180405`, for file names (sorts by time).
pub fn file_stamp(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_like_the_ui() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
        assert_eq!(format_bytes(912 * 1024), "912 KiB");
        assert_eq!(format_bytes((38.2 * 1048576.0) as u64), "38.2 MiB");
        assert_eq!(format_bytes((4.37 * 1073741824.0) as u64), "4.37 GiB");
        // 1023.9 KiB rounds to 1024: step up.
        assert_eq!(format_bytes(1024 * 1024 - 100), "1.00 MiB");
    }

    #[test]
    fn formats_counts() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1234567), "1,234,567");
    }

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        // 2026-10-22 18:04:05 UTC
        assert_eq!(format_time(1_792_692_245), "2026-10-22 18:04 UTC");
        assert_eq!(file_stamp(1_792_692_245), "20261022-180405");
        assert_eq!(file_stamp(951_782_400), "20000229-000000");
    }

    #[test]
    fn refuses_only_while_the_journal_exists() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(refuse_during_update_rollback(dir.path()), Ok(()));
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::fs::write(rollback_journal(dir.path()), "{}").unwrap();
        assert!(matches!(
            refuse_during_update_rollback(dir.path()),
            Err(Failure::Refused(m)) if m.contains("rollback.json")
        ));
    }

    #[test]
    fn new_files_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "data").unwrap();
        assert_eq!(copy_new_private(&a, &b).unwrap(), 4);
        assert!(copy_new_private(&a, &b).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&b).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        remove_if_present(&b).unwrap();
        remove_if_present(&b).unwrap();
    }
}
