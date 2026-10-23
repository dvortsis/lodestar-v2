//! The log folder as the API reads it (architecture §12.1, §10.10.12):
//! which folder, and the files in it.
//!
//! - **Which folder:** the one the file opened in this run. If file
//!   logging hasn't opened, the configured folder, only if it's an existing
//!   directory (not a link) that, on Unix, is Lodestar's account's and
//!   isn't group- or world-writable. Never created or changed from here.
//! - **Names are untrusted** (`names.rs`): only a name the writer makes
//!   is listed or read.

use std::io;
use std::path::{Path, PathBuf};

use super::names::LogName;

/// The folder the API reads, or why there's none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub dir: PathBuf,
}

/// The folder: the runtime's, else a configured one that passes the
/// checks. `Err` names why there's none.
pub fn folder(opened: Option<PathBuf>, configured: &Path) -> Result<Folder, &'static str> {
    if let Some(dir) = opened {
        return Ok(Folder { dir });
    }
    let meta = match std::fs::symlink_metadata(configured) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err("not_opened"),
        Err(_) => return Err("unreadable"),
    };
    if !meta.file_type().is_dir() {
        return Err("not_a_folder");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != rustix::process::geteuid().as_raw() || meta.mode() & 0o022 != 0 {
            return Err("not_private");
        }
    }
    Ok(Folder {
        dir: configured.to_path_buf(),
    })
}

/// A file in the folder by a log's name.
#[derive(Debug, Clone)]
pub struct Listed {
    pub name: LogName,
    /// A plain file: anything else (a link with a log's name) is never
    /// read or followed.
    pub regular: bool,
}

/// Every entry named like a log, newest first.
pub fn list(dir: &Path) -> io::Result<Vec<Listed>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        let Some(name) = e.file_name().to_str().and_then(LogName::parse) else {
            continue;
        };
        // Not followed: a link reports itself.
        let Ok(meta) = e.metadata() else { continue };
        out.push(Listed {
            name,
            regular: meta.file_type().is_file(),
        });
    }
    out.sort_by(|a, b| LogName::newest_first(&a.name, &b.name));
    // While a rotated file is compressed, its `.jsonl` and `.jsonl.gz`
    // are both there for a moment: one file, read once (the `.gz`, which
    // sorts first and holds the same lines at the same offsets).
    out.dedup_by(|later, kept| match (&later.name, &kept.name) {
        (LogName::Rotated { stamp: a, n: x, .. }, LogName::Rotated { stamp: b, n: y, .. }) => {
            a == b && x == y
        }
        _ => false,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_being_compressed_is_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "lodestar.jsonl",
            "lodestar-20261023T101500.123Z.jsonl",
            "lodestar-20261023T101500.123Z.jsonl.gz",
            "lodestar-20261023T101500.123Z.jsonl.gz.part",
            "lodestar-20251023T101500.123Z.jsonl.gz",
        ] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let names: Vec<String> = list(dir.path())
            .unwrap()
            .iter()
            .map(|l| l.name.file_name())
            .collect();
        assert_eq!(
            names,
            [
                "lodestar.jsonl",
                "lodestar-20261023T101500.123Z.jsonl.gz",
                "lodestar-20251023T101500.123Z.jsonl.gz",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn which_folder() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        assert_eq!(folder(None, &logs), Err("not_opened"));
        std::fs::create_dir(&logs).unwrap();
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(folder(None, &logs).unwrap().dir, logs);
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(folder(None, &logs), Err("not_private"));
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&logs, &link).unwrap();
        assert_eq!(folder(None, &link), Err("not_a_folder"));
        assert_eq!(
            folder(Some(link.clone()), &logs).unwrap().dir,
            link,
            "the runtime's own"
        );
    }
}
