//! Log file names are untrusted (a request path, a history cursor, the
//! details index): one is accepted only if it's exactly what the writer
//! makes, `lodestar.jsonl` or `lodestar-YYYYMMDDTHHMMSS.mmmZ[-N].jsonl[.gz]`,
//! and is parsed into a [`LogName`] that every path is built from.

use std::cmp::Ordering;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// The longest name accepted.
pub const MAX_NAME: usize = 64;

/// A log file, by what its name says.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LogName {
    /// `lodestar.jsonl`, being written.
    Current,
    /// A rotated file.
    Rotated {
        /// `YYYYMMDDTHHMMSS.mmmZ`, checked digit by digit.
        stamp: String,
        /// The `-N` a second rotation in the same millisecond adds (1:
        /// none).
        n: u32,
        gz: bool,
    },
}

impl LogName {
    pub fn parse(s: &str) -> Option<LogName> {
        if s.len() > MAX_NAME || !s.is_ascii() {
            return None;
        }
        if s == super::file::CURRENT {
            return Some(LogName::Current);
        }
        let rest = s.strip_prefix("lodestar-")?;
        let (rest, gz) = match rest.strip_suffix(".gz") {
            Some(r) => (r, true),
            None => (rest, false),
        };
        let rest = rest.strip_suffix(".jsonl")?;
        let stamp = rest.get(..20)?;
        if !stamp_ok(stamp) {
            return None;
        }
        let n = match &rest[20..] {
            "" => 1,
            tail => {
                let digits = tail.strip_prefix('-')?;
                let ok = !digits.is_empty()
                    && digits.len() <= 6
                    && !digits.starts_with('0')
                    && digits.bytes().all(|b| b.is_ascii_digit());
                let n: u32 = digits.parse().ok().filter(|_| ok)?;
                if n < 2 {
                    return None;
                }
                n
            }
        };
        Some(LogName::Rotated {
            stamp: stamp.to_string(),
            n,
            gz,
        })
    }

    pub fn file_name(&self) -> String {
        match self {
            LogName::Current => super::file::CURRENT.to_string(),
            LogName::Rotated { stamp, n, gz } => {
                let suffix = if *n > 1 {
                    format!("-{n}")
                } else {
                    String::new()
                };
                let gz = if *gz { ".gz" } else { "" };
                format!("lodestar-{stamp}{suffix}.jsonl{gz}")
            }
        }
    }

    /// The name without `.jsonl[.gz]`, for a download's file name.
    pub fn stem(&self) -> String {
        let name = self.file_name();
        name.split(".jsonl").next().unwrap_or(&name).to_string()
    }

    pub fn gz(&self) -> bool {
        matches!(self, LogName::Rotated { gz: true, .. })
    }

    /// The same file once compressed (a rotated `.jsonl` becomes
    /// `.jsonl.gz`).
    pub fn compressed(&self) -> Option<LogName> {
        match self {
            LogName::Rotated {
                stamp,
                n,
                gz: false,
            } => Some(LogName::Rotated {
                stamp: stamp.clone(),
                n: *n,
                gz: true,
            }),
            _ => None,
        }
    }

    /// A rotated file's other self: its `.gz` or its `.jsonl`.
    pub fn sibling(&self) -> Option<LogName> {
        match self {
            LogName::Current => None,
            LogName::Rotated { stamp, n, gz } => Some(LogName::Rotated {
                stamp: stamp.clone(),
                n: *n,
                gz: !gz,
            }),
        }
    }

    /// Its path in `dir`, built from the typed value.
    pub fn path(&self, dir: &Path) -> PathBuf {
        dir.join(self.file_name())
    }

    /// A rotated file's time, RFC 3339.
    pub fn rotated_at(&self) -> Option<String> {
        match self {
            LogName::Current => None,
            LogName::Rotated { stamp, .. } => Some(format!(
                "{}-{}-{}T{}:{}:{}.{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15],
                &stamp[16..19]
            )),
        }
    }

    /// Newest first: the current file, then rotated ones by time and `-N`.
    pub fn newest_first(a: &LogName, b: &LogName) -> Ordering {
        match (a, b) {
            (LogName::Current, LogName::Current) => Ordering::Equal,
            (LogName::Current, _) => Ordering::Less,
            (_, LogName::Current) => Ordering::Greater,
            (
                LogName::Rotated {
                    stamp: sa,
                    n: na,
                    gz: ga,
                },
                LogName::Rotated {
                    stamp: sb,
                    n: nb,
                    gz: gb,
                },
            ) => (sb, nb, gb).cmp(&(sa, na, ga)),
        }
    }
}

fn stamp_ok(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            8 => *c == b'T',
            15 => *c == b'.',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// Opens a log file for reading, never through a link (`O_NOFOLLOW` on
/// Unix; a reparse point is refused on Windows), and only a regular file.
/// Std's open on Windows shares delete, so a reader never makes rotation,
/// pruning or delete fail.
pub fn open_regular(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Non-blocking, so a FIFO put in a log's place can't hang the open;
        // it's refused just after as not a regular file.
        o.custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open a link itself, to refuse it.
        o.custom_flags(0x0020_0000);
    }
    let f = o.open(path)?;
    let meta = f.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        if meta.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a reparse point",
            ));
        }
    }
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn exactly_the_writers_names() {
        for good in [
            "lodestar.jsonl",
            "lodestar-20261023T101500.123Z.jsonl",
            "lodestar-20261023T101500.123Z.jsonl.gz",
            "lodestar-20261023T101500.123Z-2.jsonl",
            "lodestar-20261023T101500.123Z-17.jsonl.gz",
        ] {
            let n = LogName::parse(good).unwrap_or_else(|| panic!("{good}"));
            assert_eq!(n.file_name(), good, "round trip");
        }
        for bad in [
            "",
            "lodestar.jsonl.gz",
            "lodestar.json",
            "Lodestar.jsonl",
            "./lodestar.jsonl",
            "../lodestar.jsonl",
            "/etc/passwd",
            "lodestar-20261023T101500.123Z.jsonl.gz.part",
            "lodestar-20261023T101500.123Z-1.jsonl",
            "lodestar-20261023T101500.123Z-02.jsonl",
            "lodestar-20261023T101500.123Z-.jsonl",
            "lodestar-20261023T101500.123Z--2.jsonl",
            "lodestar-20261023T101500.123.jsonl",
            "lodestar-2026102XT101500.123Z.jsonl",
            "lodestar-20261023T101500.123Z.jsonl/../x",
            "lodestar-20261023T101500.123Z.jsonl\0",
            "lodestar-20261023T101500.123Z%2f.jsonl",
            "lodestar-20261023T101500.123Z\\.jsonl",
            "C:lodestar.jsonl",
            "index.json",
            "lodestar-20261023T101500.123Z-1234567.jsonl",
        ] {
            assert_eq!(LogName::parse(bad), None, "{bad:?}");
        }
        let r = LogName::parse("lodestar-20261023T101500.123Z-2.jsonl.gz").unwrap();
        assert_eq!(r.stem(), "lodestar-20261023T101500.123Z-2");
        assert_eq!(r.rotated_at().unwrap(), "2026-10-23T10:15:00.123Z");
        assert_eq!(LogName::Current.stem(), "lodestar");
    }

    #[test]
    fn newest_first() {
        let mut v: Vec<LogName> = [
            "lodestar-20261023T101500.123Z.jsonl.gz",
            "lodestar.jsonl",
            "lodestar-20261023T101500.123Z-2.jsonl",
            "lodestar-20251023T101500.123Z.jsonl.gz",
        ]
        .iter()
        .map(|s| LogName::parse(s).unwrap())
        .collect();
        v.sort_by(LogName::newest_first);
        let names: Vec<String> = v.iter().map(LogName::file_name).collect();
        assert_eq!(
            names,
            [
                "lodestar.jsonl",
                "lodestar-20261023T101500.123Z-2.jsonl",
                "lodestar-20261023T101500.123Z.jsonl.gz",
                "lodestar-20251023T101500.123Z.jsonl.gz",
            ]
        );
    }

    proptest! {
        /// Whatever the name, it's refused, or it resolves to exactly
        /// itself: one plain file name in the folder, never a path.
        #[test]
        fn arbitrary_names_are_refused_or_stay_in_the_folder(
            s in prop_oneof![
                any::<String>(),
                "[./\\\\:%a-z0-9A-Z\\-]{0,70}",
                "lodestar-[0-9T.Z/\\\\%\\-]{0,30}\\.jsonl(\\.gz)?",
                Just("lodestar-20261023T101500.123Z.jsonl".to_string()),
            ]
        ) {
            if let Some(name) = LogName::parse(&s) {
                let file = name.file_name();
                prop_assert_eq!(&file, &s);
                prop_assert!(file.len() <= MAX_NAME);
                prop_assert!(!file.contains(['/', '\\', ':', '\0', '%']));
                prop_assert!(!file.starts_with('.'));
                let dir = Path::new("/srv/logs");
                let p = name.path(dir);
                prop_assert_eq!(p.parent(), Some(dir));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn links_and_folders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret");
        std::fs::write(&target, "x").unwrap();
        let link = dir.path().join("lodestar.jsonl");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(open_regular(&link).is_err());
        assert!(open_regular(dir.path()).is_err());
        assert!(open_regular(&target).is_ok());
    }
}
