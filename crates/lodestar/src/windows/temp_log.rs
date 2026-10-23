//! `%SystemRoot%\Temp\lodestar-service.log`: where the Windows service and
//! its installer write the reasons a service operator needs, when there's
//! no console and no log folder to write them to.
//!
//! `%SystemRoot%\Temp` lets any local user create files, so the name may
//! already be a link or junction someone planted there. The service runs
//! as a privileged account, and following one would turn its append into a
//! write to any file the planter chose. So the file is opened with
//! `FILE_FLAG_OPEN_REPARSE_POINT` (a link is opened as itself, never
//! followed), and the opened handle is checked: anything but a regular
//! file that isn't a reparse point is refused, and nothing is written.

#[cfg(windows)]
use std::io;

/// The file's name in `%SystemRoot%\Temp`.
pub const NAME: &str = "lodestar-service.log";

/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a reparse point itself.
pub const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// `FILE_ATTRIBUTE_REPARSE_POINT`.
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// Whether an opened handle with these attributes may be written to: a
/// regular file, not a reparse point.
pub fn writable(is_file: bool, attributes: u32) -> bool {
    is_file && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
}

/// One line as the file has it: `<unix seconds> <prefix>: <why>`.
pub fn line(secs: u64, prefix: &str, why: &str) -> String {
    format!("{secs} {prefix}: {why}")
}

/// Appends `<secs> <prefix>: <why>` to `%SystemRoot%\Temp\lodestar-service.log`.
#[cfg(windows)]
pub fn append(prefix: &str, why: &str) -> io::Result<()> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SystemRoot isn't set"))?;
    append_to(
        &std::path::Path::new(&root).join("Temp").join(NAME),
        prefix,
        why,
    )
}

/// [`append`] to `path`: the tests' own file.
#[cfg(windows)]
pub fn append_to(path: &std::path::Path, prefix: &str, why: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    // The handle's own attributes: what was opened, not what the name
    // pointed to a moment before.
    let meta = file.metadata()?;
    if !writable(meta.is_file(), meta.file_attributes()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the service log is a link or not a regular file",
        ));
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    writeln!(file, "{}", line(secs, prefix, why))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_regular_file_that_isnt_a_reparse_point_is_written() {
        assert!(writable(true, 0x20));
        assert!(!writable(true, FILE_ATTRIBUTE_REPARSE_POINT | 0x20));
        assert!(!writable(false, 0x10));
        assert_eq!(line(7, "logging", "x"), "7 logging: x");
    }

    #[cfg(windows)]
    fn run(program: &str, args: &[&std::ffi::OsStr]) -> bool {
        let root = std::env::var_os("SystemRoot").unwrap();
        std::process::Command::new(std::path::Path::new(&root).join("System32").join(program))
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[cfg(windows)]
    #[test]
    fn appends_to_a_regular_file_and_refuses_links() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(NAME);
        append_to(&file, "logging", "first").unwrap();
        append_to(&file, "service refused to start", "second").unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(" logging: first\n"), "{text}");
        assert!(
            text.contains(" service refused to start: second\n"),
            "{text}"
        );

        // A file symlink (needs Developer Mode or an elevated runner) and a
        // junction, each named like the log and pointing at a target:
        // refused, and the target is untouched.
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "untouched").unwrap();
        let link_dir = dir.path().join("links");
        std::fs::create_dir(&link_dir).unwrap();
        let link = link_dir.join(NAME);
        if std::os::windows::fs::symlink_file(&target, &link).is_ok() {
            assert!(append_to(&link, "logging", "planted").is_err());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
            std::fs::remove_file(&link).unwrap();
        }
        let target_dir = dir.path().join("target-dir");
        std::fs::create_dir(&target_dir).unwrap();
        assert!(run(
            "cmd.exe",
            &[
                "/C".as_ref(),
                "mklink".as_ref(),
                "/J".as_ref(),
                link.as_os_str(),
                target_dir.as_os_str(),
            ]
        ));
        assert!(append_to(&link, "logging", "planted").is_err());
        assert_eq!(std::fs::read_dir(&target_dir).unwrap().count(), 0);
    }
}
