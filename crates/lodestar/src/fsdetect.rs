//! Is the data directory on a filesystem that's slow or unsafe for
//! SQLite's WAL and the index's memory-mapped segments (architecture §11)?
//! Unraid's `/mnt/user` goes through the FUSE `shfs` layer, and NFS or
//! SMB/CIFS shares don't give SQLite the locking it relies on.
//!
//! Linux reads the mount table (`/proc/self/mountinfo`) rather than
//! calling `statfs`: the workspace forbids `unsafe`, and the mount table
//! names the type of a Docker bind mount's source too (a container's
//! `/data` mapped from `/mnt/user/appdata` shows as `fuse.shfs`). Windows
//! recognises network drives by their canonical path: a share is
//! `\\server\share`, and a mapped drive letter canonicalises to the same
//! `\\?\UNC\…` form.
//!
//! [`detect`] is public for `/stats` and the first-run wizard; the
//! server only logs [`warn_at_start`] for now.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// What kind of filesystem holds a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FsKind {
    /// A local disk, or a filesystem with nothing known against it.
    Local,
    /// Unraid's user share (`/mnt/user`, FUSE `shfs`).
    UnraidUserShare,
    /// Another FUSE filesystem (mergerfs, rclone, sshfs, ntfs-3g, …).
    Fuse,
    /// A network filesystem: NFS, SMB/CIFS, a Windows network drive, ….
    Network,
    /// The platform gives no answer (no mount table, say).
    Unknown,
}

/// The data directory's filesystem, as [`detect`] found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsReport {
    /// The directory checked, canonicalised when it exists.
    pub path: PathBuf,
    pub kind: FsKind,
    /// The mount table's type (`ext4`, `fuse.shfs`, `nfs4`, …), if known.
    pub fs_type: Option<String>,
    /// Where that filesystem is mounted, if known.
    pub mount_point: Option<PathBuf>,
}

impl FsReport {
    /// Slow or unsafe for the store and index.
    pub fn is_risky(&self) -> bool {
        matches!(
            self.kind,
            FsKind::UnraidUserShare | FsKind::Fuse | FsKind::Network
        )
    }

    /// What's wrong and the safe alternative, for the log, `/stats` and
    /// the wizard; `None` when nothing is.
    pub fn warning(&self) -> Option<String> {
        let path = self.path.display();
        let fs = self
            .fs_type
            .as_deref()
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        Some(match self.kind {
            FsKind::UnraidUserShare => format!(
                "The data folder {path} is on Unraid's user share (/mnt/user, FUSE shfs){fs}. \
                 It's slow for the search index and risky for the database. Map the data folder \
                 to a pool path instead, such as /mnt/cache/appdata/lodestar, and keep only \
                 backups on the array."
            ),
            FsKind::Fuse => format!(
                "The data folder {path} is on a FUSE filesystem{fs}. It's slow for the search \
                 index and may not give the database the locking it needs. Keep the data folder \
                 on a local disk, ideally an SSD."
            ),
            FsKind::Network => format!(
                "The data folder {path} is on a network filesystem{fs}. Its file locking isn't \
                 reliable enough for the database, which can be damaged. Keep the data folder \
                 on a local disk, ideally an SSD, and send only backups to the share."
            ),
            FsKind::Local | FsKind::Unknown => return None,
        })
    }
}

/// Which filesystem holds `dir` (or its nearest existing parent).
pub fn detect(dir: &Path) -> FsReport {
    let path = canonical(dir);
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string("/proc/self/mountinfo") {
            Ok(table) => from_mountinfo(&path, &table),
            Err(_) => by_path(path),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        by_path(path)
    }
}

/// Logs [`FsReport::warning`] for the data directory, if there is one.
pub fn warn_at_start(dir: &Path) {
    let report = detect(dir);
    if report.is_risky() {
        tracing::warn!("{}", report.warning().unwrap_or_default());
    }
}

/// The canonical form of `dir`, or of its nearest existing parent with
/// the rest appended (the data directory may not exist yet).
fn canonical(dir: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = dir;
    loop {
        if let Ok(c) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(c, |p, name| p.join(name));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = if parent.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    parent
                };
            }
            _ => return dir.to_path_buf(),
        }
    }
}

/// What the path alone says: a Windows share, or Unraid's user share by
/// its well-known location.
fn by_path(path: PathBuf) -> FsReport {
    let kind = if is_unc(&path) {
        FsKind::Network
    } else if under_unraid_user(&path) {
        FsKind::UnraidUserShare
    } else if cfg!(windows) {
        FsKind::Local
    } else {
        FsKind::Unknown
    };
    FsReport {
        path,
        kind,
        fs_type: None,
        mount_point: None,
    }
}

/// `\\server\share\…` or its verbatim form `\\?\UNC\server\share\…`.
fn is_unc(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    let s = s.replace('/', "\\");
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return rest
            .as_bytes()
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(br"UNC\"));
    }
    // `\\.\` is a device path, not a share.
    s.starts_with(r"\\") && !s.starts_with(r"\\.\")
}

/// `/mnt/user/…` or `/mnt/user0/…` on an Unraid host.
fn under_unraid_user(path: &Path) -> bool {
    let mut c = path.components();
    matches!(
        (c.next(), c.next(), c.next()),
        (Some(Component::RootDir), Some(Component::Normal(m)), Some(Component::Normal(u)))
            if m == "mnt" && (u == "user" || u == "user0")
    )
}

/// Classifies `path` by the longest mount point in `table` (the format of
/// `/proc/self/mountinfo`) that contains it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn from_mountinfo(path: &Path, table: &str) -> FsReport {
    let best = table
        .lines()
        .filter_map(parse_mountinfo_line)
        .filter(|(mount, _)| path.starts_with(mount))
        .max_by_key(|(mount, _)| mount.components().count());
    let Some((mount, fs_type)) = best else {
        return by_path(path.to_path_buf());
    };
    let mut kind = classify(&fs_type);
    // A host path under /mnt/user is shfs even if the type is unusual,
    // unless its mount is itself under /mnt/user: an Unraid 6.12+
    // exclusive share, a bind mount of the pool that bypasses shfs.
    if kind == FsKind::Local && under_unraid_user(path) && !under_unraid_user(&mount) {
        kind = FsKind::UnraidUserShare;
    }
    FsReport {
        path: path.to_path_buf(),
        kind,
        fs_type: Some(fs_type),
        mount_point: Some(mount),
    }
}

/// `(mount point, fs type)` from one mountinfo line:
/// `id parent major:minor root mount-point options [optional…] - type source super`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_mountinfo_line(line: &str) -> Option<(PathBuf, String)> {
    let mut fields = line.split(' ');
    let mount = fields.nth(4)?;
    let mut fields = fields.skip_while(|f| *f != "-");
    fields.next()?;
    let fs_type = fields.next().filter(|t| !t.is_empty())?;
    Some((PathBuf::from(unescape(mount)), fs_type.to_string()))
}

/// The kernel escapes space, tab, newline and backslash as `\ooo`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = b.get(i + 1..i + 4).filter(|d| {
            b[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)) && d[0] <= b'3'
        });
        match octal {
            Some(d) => {
                out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn classify(fs_type: &str) -> FsKind {
    const NETWORK: &[&str] = &[
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "smbfs",
        "ncpfs",
        "afs",
        "ceph",
        "9p",
        "lustre",
        "gpfs",
        "glusterfs",
        "ocfs2",
        "gfs2",
        "coda",
    ];
    match fs_type {
        "fuse.shfs" => FsKind::UnraidUserShare,
        // Network filesystems mounted through FUSE are network first.
        "fuse.sshfs" | "fuse.rclone" | "fuse.glusterfs" | "fuse.s3fs" | "fuse.davfs" => {
            FsKind::Network
        }
        t if t == "fuse" || t == "fuseblk" || t.starts_with("fuse.") => FsKind::Fuse,
        t if NETWORK.contains(&t) => FsKind::Network,
        _ => FsKind::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNRAID_CONTAINER: &str = "\
1411 1210 0:388 / / rw,relatime master:584 - overlay overlay rw,lowerdir=/var/lib/docker/overlay2/l/X
1412 1411 0:390 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw
1420 1411 0:57 /appdata/lodestar /data rw,noatime - fuse.shfs shfs rw,user_id=0,group_id=0,allow_other
1421 1411 0:58 /appdata/lodestar/hot /data/hot rw,noatime - btrfs /dev/nvme0n1p1 rw,ssd
1422 1411 0:59 / /mnt/my\\040share rw - cifs //nas/share rw,vers=3.0
1423 1411 0:60 / /srv/nfs rw shared:7 master:3 - nfs4 nas:/export rw
";

    fn at(path: &str) -> FsReport {
        from_mountinfo(Path::new(path), UNRAID_CONTAINER)
    }

    #[test]
    fn unraid_user_share_through_a_bind_mount() {
        let r = at("/data");
        assert_eq!(r.kind, FsKind::UnraidUserShare);
        assert_eq!(r.fs_type.as_deref(), Some("fuse.shfs"));
        assert_eq!(r.mount_point.as_deref(), Some(Path::new("/data")));
        assert!(r.is_risky());
        let w = r.warning().unwrap();
        assert!(w.contains("/mnt/cache/appdata/lodestar"), "{w}");
        // The longest containing mount wins: /data/hot is on the pool.
        assert_eq!(at("/data/hot/x").kind, FsKind::Local);
        assert_eq!(at("/data/hotter").kind, FsKind::UnraidUserShare);
    }

    #[test]
    fn network_and_local_mounts() {
        let smb = at("/mnt/my share/lodestar");
        assert_eq!(smb.kind, FsKind::Network);
        assert_eq!(smb.mount_point.as_deref(), Some(Path::new("/mnt/my share")));
        assert_eq!(at("/srv/nfs/data").fs_type.as_deref(), Some("nfs4"));
        assert_eq!(at("/srv/nfs/data").kind, FsKind::Network);
        let local = at("/var/lib/lodestar");
        assert_eq!(local.kind, FsKind::Local);
        assert_eq!(local.fs_type.as_deref(), Some("overlay"));
        assert!(!local.is_risky());
        assert_eq!(local.warning(), None);
        // On the Unraid host itself, /mnt/user is shfs by location too.
        assert_eq!(
            from_mountinfo(
                Path::new("/mnt/user/appdata/x"),
                "1 0 8:1 / / rw - xfs /dev/sda1 rw"
            )
            .kind,
            FsKind::UnraidUserShare
        );
        // An exclusive share is a bind mount of the pool: fine.
        let exclusive = "1 0 8:1 / / rw - xfs /dev/sda1 rw\n\
                         2 1 0:30 /appdata /mnt/user/appdata rw - btrfs /dev/nvme0n1p1 rw";
        assert_eq!(
            from_mountinfo(Path::new("/mnt/user/appdata/lodestar"), exclusive).kind,
            FsKind::Local
        );
    }

    #[test]
    fn classifies_types() {
        assert_eq!(classify("ext4"), FsKind::Local);
        assert_eq!(classify("zfs"), FsKind::Local);
        assert_eq!(classify("fuse.mergerfs"), FsKind::Fuse);
        assert_eq!(classify("fuseblk"), FsKind::Fuse);
        assert_eq!(classify("fuse.sshfs"), FsKind::Network);
        assert_eq!(classify("smb3"), FsKind::Network);
        assert_eq!(classify("9p"), FsKind::Network);
        assert_eq!(classify("fusectl"), FsKind::Local);
    }

    #[test]
    fn malformed_lines_are_skipped() {
        assert_eq!(parse_mountinfo_line(""), None);
        assert_eq!(parse_mountinfo_line("1 2 3 / /x rw"), None);
        assert_eq!(parse_mountinfo_line("1 2 3 / /x rw -"), None);
        assert_eq!(
            parse_mountinfo_line("1 2 3 / /x rw a:1 b:2 - ext4 /dev/x rw"),
            Some((PathBuf::from("/x"), "ext4".into()))
        );
        // No containing mount at all: fall back to the path.
        assert_eq!(
            from_mountinfo(Path::new("/a"), "junk").kind,
            by_path("/a".into()).kind
        );
    }

    proptest::proptest! {
        // The mount table is the kernel's, but a parser never panics.
        #[test]
        fn parsers_never_panic(table in "[ -~\n]{0,400}|\\PC{0,200}", path in "\\PC{0,60}") {
            let _ = from_mountinfo(Path::new(&path), &table);
            for line in table.lines() {
                let _ = parse_mountinfo_line(line);
            }
            let _ = unescape(&table);
            let _ = is_unc(Path::new(&path));
        }

        #[test]
        fn unescape_inverts_the_kernels_escaping(s in "\\PC{0,80}") {
            let mut escaped = String::new();
            for c in s.chars() {
                match c {
                    ' ' | '\t' | '\n' | '\\' => escaped.push_str(&format!("\\{:03o}", c as u32)),
                    c => escaped.push(c),
                }
            }
            proptest::prop_assert_eq!(unescape(&escaped), s);
        }
    }

    #[test]
    fn unescapes_octal() {
        assert_eq!(unescape(r"/a\040b\011c\134d"), "/a b\tc\\d");
        assert_eq!(unescape(r"/a\9x\04"), r"/a\9x\04");
    }

    #[test]
    fn windows_shares_by_path() {
        assert!(is_unc(Path::new(r"\\nas\share\lodestar")));
        assert!(is_unc(Path::new(r"\\?\UNC\nas\share\lodestar")));
        assert!(is_unc(Path::new(r"\\?\unc\nas\share")));
        assert!(!is_unc(Path::new(r"\\?\C:\ProgramData\Lodestar")));
        assert!(!is_unc(Path::new(r"\\.\PhysicalDrive0")));
        assert!(!is_unc(Path::new(r"C:\ProgramData\Lodestar")));
        assert!(!is_unc(Path::new("/data")));
        assert_eq!(by_path(r"\\nas\share".into()).kind, FsKind::Network);
    }

    #[test]
    fn unraid_paths() {
        assert!(under_unraid_user(Path::new("/mnt/user/appdata")));
        assert!(under_unraid_user(Path::new("/mnt/user0/appdata")));
        assert!(!under_unraid_user(Path::new("/mnt/cache/appdata")));
        assert!(!under_unraid_user(Path::new("/mnt/username")));
        assert!(!under_unraid_user(Path::new("mnt/user")));
    }

    #[test]
    fn a_missing_directory_resolves_through_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("a").join("b");
        let c = canonical(&missing);
        assert_eq!(
            c,
            std::fs::canonicalize(dir.path())
                .unwrap()
                .join("a")
                .join("b")
        );
        // The live table answers for a real directory without panicking.
        let r = detect(&missing);
        assert_eq!(r.path, c);
    }
}
