//! Extracting a companion's archive (plan 2026-11-06-v06-companions §4
//! CMP-2, Q16; architecture §12, §18.10).
//!
//! The archive is untrusted input even after CMP-1 has checked its pin:
//! a pin says the bytes are the ones upstream published, not that
//! upstream's archive is safe to unpack. So extraction is streaming and
//! bounded, and it **refuses the whole archive** at the first entry that
//! breaks a rule. It never skips an entry and carries on.
//!
//! - **Names** ([`normalise`]): printable ASCII only, `/` the only
//!   separator, nothing absolute (`/x`, `C:`, `//host`), no `..`, `.` or
//!   empty component, no backslash, nothing Windows can't hold safely
//!   (`:`, `<>"|?*`, device names such as `CON` or `com1.txt`, a trailing
//!   dot or space), at most [`MAX_COMPONENT`] bytes a component,
//!   [`MAX_DEPTH`] components and [`MAX_NAME`] bytes. One leading `./`
//!   (what `tar -C dir .` writes) is dropped.
//! - **Duplicates** compare after that normalisation and ASCII
//!   case-folding, so two names that would be one file on a
//!   case-insensitive filesystem are refused. A directory may be listed
//!   after files inside it created it; listed twice, it's a duplicate.
//! - **Kinds:** regular files and directories only. Symlinks, hard links,
//!   devices, FIFOs and sockets are refused, as are setuid, setgid and
//!   sticky bits, encrypted zip entries and any other tar entry type.
//! - **Caps (Q16):** [`MAX_ENTRIES`] entries; [`MAX_TOTAL`] bytes in all,
//!   counted as they're written (one byte past stops it, whatever a header
//!   claimed); and [`MAX_RATIO`]:1, checked as bytes are written, for the
//!   entry and for the archive. An entry whose written size differs from
//!   its header's is refused. Directories a name creates without listing
//!   them count toward the entry cap too. Everything else a tar.gz
//!   decompresses (headers, long names, pax records, skipped bodies) is
//!   bounded on its own by [`MAX_OVERHEAD`], so no header can make the
//!   parser hold more than that in memory (about twice that at peak). A
//!   zip's index is read under the same budget, so an index of millions of
//!   records is refused before it's in memory.
//! - **Writing:** every file is created new (`O_CREAT|O_EXCL` on Unix,
//!   `CREATE_NEW` on Windows), which never opens through an existing
//!   symlink. Every directory is one this extraction created into a
//!   staging directory it created itself, so no path can traverse anything
//!   else. Files are 0644 and directories 0755; only the catalogue's
//!   executable is 0755. On Windows they inherit the companion folder's
//!   ACL. Each file is fsynced when written, and each directory at the end.
//! - **The executable** the catalogue names must be in the archive as a
//!   regular file, or the archive is refused.
//! - **Failure leaves nothing behind:** [`extract`] removes the staging
//!   directory on any error. Errors are typed and carry no attacker text
//!   beyond an [`EntryName`], which is bounded and sanitised.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::catalogue::{ArchiveKind, RelPath};

/// The most entries an archive may have (Q16). Every entry counts,
/// directories and tar's global headers included.
pub const MAX_ENTRIES: usize = 20_000;

/// The most bytes an archive may extract to, in all (Q16): 2 GiB.
pub const MAX_TOTAL: u64 = 2 * 1024 * 1024 * 1024;

/// The highest compression ratio allowed (Q16), for each entry and for
/// the whole archive.
pub const MAX_RATIO: u64 = 100;

/// Output up to this size is never refused for its ratio alone: a small
/// file of one repeated byte can pass 100:1 honestly, and the deflate and
/// gzip framing make the ratio of a few kilobytes meaningless. Beyond it,
/// every byte written is checked against [`MAX_RATIO`].
pub const RATIO_FLOOR: u64 = 64 * 1024;

/// The caps one extraction runs under. Shipped code always uses
/// [`Limits::Q16`]; smaller ones exist only for tests and the fuzz target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub entries: usize,
    pub total: u64,
    pub ratio: u64,
    pub ratio_floor: u64,
    /// What a tar.gz may decompress beyond the files it writes: headers,
    /// long names, pax records, padding and skipped bodies.
    pub overhead: u64,
}

impl Limits {
    /// Plan Q16's caps.
    pub const Q16: Limits = Limits {
        entries: MAX_ENTRIES,
        total: MAX_TOTAL,
        ratio: MAX_RATIO,
        ratio_floor: RATIO_FLOOR,
        overhead: MAX_OVERHEAD,
    };

    /// `written` bytes from `compressed` is within the ratio, or still
    /// under its floor.
    fn ratio_ok(&self, written: u64, compressed: u64) -> bool {
        written <= self.ratio_floor || written <= compressed.saturating_mul(self.ratio)
    }
}

/// What a tar.gz may decompress beyond its files' bytes (headers, long
/// names, pax records, padding, skipped bodies): 64 MiB. [`MAX_ENTRIES`]
/// tiny files, each with a header, a pax record and padding, need about
/// 40 MiB.
pub const MAX_OVERHEAD: u64 = 64 * 1024 * 1024;

/// The longest entry name, in bytes.
pub const MAX_NAME: usize = 1024;

/// The longest path component, in bytes (what NTFS and ext4 hold).
pub const MAX_COMPONENT: usize = 255;

/// The most components in a name.
pub const MAX_DEPTH: usize = 64;

/// The most bytes of an entry name an error carries.
const SHOWN_NAME: usize = 96;

/// The gzip reader's buffer. Compressed bytes are counted as they're read
/// from the archive, so up to this many may be ahead of what's decoded;
/// an entry's ratio allows for it.
const READ_AHEAD: usize = 32 * 1024;

/// The most bytes allowed after a tar's end, before the gzip trailer
/// (tar pads to a 10 KiB record; some writers add more zeros).
const MAX_TRAILER: u64 = 1024 * 1024;

const COPY_BUF: usize = 64 * 1024;

// ─── Errors ─────────────────────────────────────────────────────────────

/// Why an archive was refused. Every reason refuses the whole archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Refusal {
    /// An absolute name: a leading `/`, a drive letter or a UNC prefix.
    Absolute,
    /// A `..` component.
    Traversal,
    /// An empty component (`a//b`) or an empty name.
    EmptyComponent,
    /// A `.` component anywhere but one leading `./`.
    DotComponent,
    /// A backslash, which Windows treats as a separator.
    Backslash,
    /// A `:` (an NTFS alternate data stream).
    Colon,
    /// A control or non-ASCII byte, or one of `<>"|?*`.
    Character,
    /// A Windows device name, such as `CON`, `nul.txt` or `COM1`.
    ReservedName,
    /// A component ending in a dot or a space, which Windows trims.
    TrailingDotOrSpace,
    /// A name, or one of its components, over the length or depth cap.
    NameTooLong,
    /// Two entries that are one file after normalisation and case-folding.
    Duplicate,
    /// An entry under a name that is a file, not a directory.
    NotADirectory,
    /// A symlink or a hard link.
    Link,
    /// A device, FIFO, socket or any other non-regular entry.
    Special,
    /// A setuid, setgid or sticky bit.
    ModeBits,
    /// An encrypted zip entry.
    Encrypted,
    /// More than [`MAX_ENTRIES`] entries.
    TooManyEntries,
    /// More than [`MAX_TOTAL`] bytes written.
    TooLarge,
    /// A compression ratio over [`MAX_RATIO`]:1.
    Ratio,
    /// An entry whose bytes differ in number from its header's size.
    SizeMismatch,
    /// The catalogue's executable isn't in the archive.
    MissingExecutable,
    /// The catalogue's executable is in the archive, but as a directory.
    ExecutableNotFile,
}

impl Refusal {
    /// A stable code for logs and the API.
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Absolute => "absolute",
            Refusal::Traversal => "traversal",
            Refusal::EmptyComponent => "empty_component",
            Refusal::DotComponent => "dot_component",
            Refusal::Backslash => "backslash",
            Refusal::Colon => "colon",
            Refusal::Character => "character",
            Refusal::ReservedName => "reserved_name",
            Refusal::TrailingDotOrSpace => "trailing_dot_or_space",
            Refusal::NameTooLong => "name_too_long",
            Refusal::Duplicate => "duplicate",
            Refusal::NotADirectory => "not_a_directory",
            Refusal::Link => "link",
            Refusal::Special => "special",
            Refusal::ModeBits => "mode_bits",
            Refusal::Encrypted => "encrypted",
            Refusal::TooManyEntries => "too_many_entries",
            Refusal::TooLarge => "too_large",
            Refusal::Ratio => "ratio",
            Refusal::SizeMismatch => "size_mismatch",
            Refusal::MissingExecutable => "missing_executable",
            Refusal::ExecutableNotFile => "executable_not_file",
        }
    }
}

/// An entry's name as an error shows it: at most 96 bytes, with every
/// byte that isn't printable ASCII shown as `?`. Never used as a path.
#[derive(Clone, PartialEq, Eq)]
pub struct EntryName(String);

impl EntryName {
    fn new(raw: &[u8]) -> EntryName {
        let mut s: String = raw
            .iter()
            .take(SHOWN_NAME)
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '?'
                }
            })
            .collect();
        if raw.len() > SHOWN_NAME {
            s.push('…');
        }
        EntryName(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EntryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// Why extraction failed. The staging directory is gone unless
/// [`ExtractError::Cleanup`] says otherwise.
#[derive(Debug)]
#[non_exhaustive]
pub enum ExtractError {
    /// The archive broke a rule. `entry` names the entry, when one did.
    Refused {
        refusal: Refusal,
        entry: Option<EntryName>,
    },
    /// The archive couldn't be read: truncated, corrupt, a checksum that
    /// doesn't match, or a compression method other than deflate or
    /// stored. The parser's own message isn't kept (it can quote the
    /// archive).
    Corrupt,
    /// The kind isn't one this module extracts (a bare binary or an
    /// installer).
    NotAnArchive(ArchiveKind),
    /// The staging directory already exists and isn't a plain directory
    /// this module may clean.
    StagingInTheWay,
    /// Writing to the disk failed.
    Io(io::ErrorKind),
    /// Extraction failed with `cause`, then removing the staging directory
    /// failed too, so it may still be there. The next stage cleans it.
    Cleanup {
        cause: Box<ExtractError>,
        kind: io::ErrorKind,
    },
}

impl ExtractError {
    fn refused(refusal: Refusal, entry: &[u8]) -> ExtractError {
        ExtractError::Refused {
            refusal,
            entry: Some(EntryName::new(entry)),
        }
    }

    /// The refusal, if the archive broke a rule.
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            ExtractError::Refused { refusal, .. } => Some(*refusal),
            ExtractError::Cleanup { cause, .. } => cause.refusal(),
            _ => None,
        }
    }

    /// A stable code for logs and the API.
    pub fn code(&self) -> &'static str {
        match self {
            ExtractError::Refused { refusal, .. } => refusal.code(),
            ExtractError::Corrupt => "corrupt",
            ExtractError::NotAnArchive(_) => "not_an_archive",
            ExtractError::StagingInTheWay => "staging_in_the_way",
            ExtractError::Io(_) => "io",
            ExtractError::Cleanup { .. } => "cleanup",
        }
    }
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExtractError::Refused {
                refusal,
                entry: Some(e),
            } => write!(f, "archive refused ({}) at {:?}", refusal.code(), e.0),
            ExtractError::Refused { refusal, .. } => {
                write!(f, "archive refused ({})", refusal.code())
            }
            ExtractError::Corrupt => f.write_str("archive is corrupt or truncated"),
            ExtractError::NotAnArchive(k) => write!(f, "{k:?} isn't an archive kind"),
            ExtractError::StagingInTheWay => {
                f.write_str("the staging path exists and isn't a plain directory")
            }
            ExtractError::Io(k) => write!(f, "writing the archive failed: {k}"),
            ExtractError::Cleanup { cause, kind } => {
                write!(f, "{cause}; removing the staging directory failed: {kind}")
            }
        }
    }
}

impl std::error::Error for ExtractError {}

fn io_err(e: io::Error) -> ExtractError {
    ExtractError::Io(e.kind())
}

// ─── Names ──────────────────────────────────────────────────────────────

/// An entry name that passed [`normalise`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalised {
    /// The components, in order. Empty for the archive's root (`./`).
    pub components: Vec<String>,
    /// Whether the name ended in `/`.
    pub trailing_slash: bool,
}

impl Normalised {
    /// The name `/`-joined, as the catalogue's [`RelPath`] is written.
    pub fn joined(&self) -> String {
        self.components.join("/")
    }

    /// The key duplicates compare by: the joined name, ASCII lower-cased.
    pub fn folded(&self) -> String {
        self.joined().to_ascii_lowercase()
    }
}

/// A Windows device name, with or without an extension, and allowing for
/// the spaces Windows trims before one (`nul .txt`).
fn reserved(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit())
}

/// Checks and normalises one raw entry name (module docs, "Names").
pub fn normalise(raw: &[u8]) -> Result<Normalised, Refusal> {
    if raw.is_empty() {
        return Err(Refusal::EmptyComponent);
    }
    if raw.len() > MAX_NAME {
        return Err(Refusal::NameTooLong);
    }
    if raw.contains(&b'\\') {
        return Err(Refusal::Backslash);
    }
    if raw[0] == b'/' || (raw.len() >= 2 && raw[0].is_ascii_alphabetic() && raw[1] == b':') {
        return Err(Refusal::Absolute);
    }
    if raw.contains(&b':') {
        return Err(Refusal::Colon);
    }
    if raw.iter().any(|&b| {
        !(0x20..0x7f).contains(&b) || matches!(b, b'<' | b'>' | b'"' | b'|' | b'?' | b'*')
    }) {
        return Err(Refusal::Character);
    }
    // Printable ASCII from here on, so this can't fail.
    let s = std::str::from_utf8(raw).map_err(|_| Refusal::Character)?;
    let (s, trailing_slash) = match s.strip_suffix('/') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    let s = match s {
        "." => "",
        _ => s.strip_prefix("./").unwrap_or(s),
    };
    if s.is_empty() {
        return if trailing_slash || raw == b"." {
            Ok(Normalised {
                components: Vec::new(),
                trailing_slash,
            })
        } else {
            Err(Refusal::EmptyComponent)
        };
    }
    let mut components = Vec::new();
    for c in s.split('/') {
        match c {
            "" => return Err(Refusal::EmptyComponent),
            "." => return Err(Refusal::DotComponent),
            ".." => return Err(Refusal::Traversal),
            _ => {}
        }
        if c.len() > MAX_COMPONENT {
            return Err(Refusal::NameTooLong);
        }
        if c.ends_with('.') || c.ends_with(' ') {
            return Err(Refusal::TrailingDotOrSpace);
        }
        if reserved(c) {
            return Err(Refusal::ReservedName);
        }
        components.push(c.to_owned());
        if components.len() > MAX_DEPTH {
            return Err(Refusal::NameTooLong);
        }
    }
    Ok(Normalised {
        components,
        trailing_slash,
    })
}

// ─── The entry point ────────────────────────────────────────────────────

/// What a successful extraction wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// Entries read, directories and headers included.
    pub entries: usize,
    /// Regular files written.
    pub files: usize,
    /// Bytes written, in all.
    pub bytes: u64,
}

/// An archive to read: zip needs to seek (its index is at the end);
/// tar.gz is read once, front to back.
pub trait Archive: Read + Seek {}
impl<T: Read + Seek> Archive for T {}

/// Extracts `archive` (of the `kind` given) into `staging`,
/// which this creates and which must not exist. `executable` is the
/// catalogue's program inside the archive.
///
/// On success every file and directory under `staging` is fsynced; the
/// caller renames it into place ([`crate::layout`]). On any error
/// `staging` is removed before this returns.
pub fn extract(
    kind: ArchiveKind,
    archive: &mut dyn Archive,
    executable: &RelPath,
    staging: &Path,
) -> Result<Extracted, ExtractError> {
    extract_with(Limits::Q16, kind, archive, executable, staging)
}

/// [`extract`] under other caps, for tests and the fuzz target only.
#[cfg(any(test, feature = "testing"))]
pub fn extract_limited(
    limits: Limits,
    kind: ArchiveKind,
    archive: &mut dyn Archive,
    executable: &RelPath,
    staging: &Path,
) -> Result<Extracted, ExtractError> {
    extract_with(limits, kind, archive, executable, staging)
}

fn extract_with(
    limits: Limits,
    kind: ArchiveKind,
    archive: &mut dyn Archive,
    executable: &RelPath,
    staging: &Path,
) -> Result<Extracted, ExtractError> {
    if !matches!(kind, ArchiveKind::Zip | ArchiveKind::TarGz) {
        return Err(ExtractError::NotAnArchive(kind));
    }
    // The archive's real length, the whole-archive ratio's denominator:
    // measured, never taken from the caller or a header.
    let len = archive
        .seek(SeekFrom::End(0))
        .and_then(|len| archive.seek(SeekFrom::Start(0)).map(|_| len))
        .map_err(io_err)?;
    create_dir(staging).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            ExtractError::StagingInTheWay
        } else {
            io_err(e)
        }
    })?;
    let mut out = Out::new(limits, staging, executable);
    let result = match kind {
        ArchiveKind::Zip => zip_entries(archive, len, &mut out),
        _ => tar_gz_entries(archive, &mut out),
    }
    .and_then(|()| out.finish());
    match result {
        Ok(done) => Ok(done),
        Err(cause) => match fs::remove_dir_all(staging) {
            Ok(()) => Err(cause),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(cause),
            Err(e) => Err(ExtractError::Cleanup {
                cause: Box::new(cause),
                kind: e.kind(),
            }),
        },
    }
}

// ─── Writing ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    /// A directory: `listed` if the archive listed it (not only created
    /// because a later name needed it), and `rel`, its path as spelt on
    /// disk (the first spelling seen, when later names fold onto it).
    Dir {
        listed: bool,
        rel: String,
    },
    File,
}

/// The output side: what's been written, keyed by folded name, and the
/// running totals the caps check.
struct Out<'a> {
    limits: Limits,
    root: &'a Path,
    executable: &'a RelPath,
    nodes: HashMap<String, Node>,
    /// Directories created, in creation order (parents first).
    dirs: Vec<PathBuf>,
    entries: usize,
    files: usize,
    total: u64,
    /// The same running total, shared with a tar.gz's [`Guard`], which
    /// counts everything else it decompresses as overhead.
    body: Rc<Cell<u64>>,
    /// Whether the catalogue's executable, spelt exactly, was written.
    executable_written: bool,
}

impl<'a> Out<'a> {
    fn new(limits: Limits, root: &'a Path, executable: &'a RelPath) -> Out<'a> {
        Out {
            limits,
            root,
            executable,
            nodes: HashMap::new(),
            dirs: Vec::new(),
            entries: 0,
            files: 0,
            total: 0,
            body: Rc::new(Cell::new(0)),
            executable_written: false,
        }
    }

    /// Counts one entry, or one directory a name created without listing
    /// it, against [`MAX_ENTRIES`].
    fn count(&mut self, raw: &[u8]) -> Result<(), ExtractError> {
        self.entries += 1;
        if self.entries > self.limits.entries {
            return Err(ExtractError::refused(Refusal::TooManyEntries, raw));
        }
        Ok(())
    }

    /// Makes sure every parent of `name` is a directory this extraction
    /// created, creating any that are missing, and returns the parent's
    /// path relative to the root as spelt on disk.
    fn parents(&mut self, name: &Normalised, raw: &[u8]) -> Result<String, ExtractError> {
        let n = name.components.len().saturating_sub(1);
        let mut key = String::new();
        let mut rel = String::new();
        for c in &name.components[..n] {
            if !key.is_empty() {
                key.push('/');
            }
            key.push_str(&c.to_ascii_lowercase());
            match self.nodes.get(&key) {
                Some(Node::File) => return Err(ExtractError::refused(Refusal::NotADirectory, raw)),
                Some(Node::Dir { rel: on_disk, .. }) => rel = on_disk.clone(),
                None => {
                    // A deep name creates many folders from one entry;
                    // each counts, so the cap bounds what's on disk.
                    self.count(raw)?;
                    rel = join(&rel, c);
                    let p = self.at(&rel);
                    create_dir(&p).map_err(|e| self.create_failed(e, raw))?;
                    self.nodes.insert(
                        key.clone(),
                        Node::Dir {
                            listed: false,
                            rel: rel.clone(),
                        },
                    );
                    self.dirs.push(p);
                }
            }
        }
        Ok(rel)
    }

    /// The path of `rel` (`/`-separated) under the root.
    fn at(&self, rel: &str) -> PathBuf {
        let mut p = self.root.to_path_buf();
        p.extend(rel.split('/'));
        p
    }

    /// A create that failed because the name is taken was a collision the
    /// folding didn't predict (an 8.3 short name, say): a duplicate.
    fn create_failed(&self, e: io::Error, raw: &[u8]) -> ExtractError {
        if e.kind() == io::ErrorKind::AlreadyExists {
            ExtractError::refused(Refusal::Duplicate, raw)
        } else {
            io_err(e)
        }
    }

    fn dir(&mut self, name: &Normalised, raw: &[u8]) -> Result<(), ExtractError> {
        if name.components.is_empty() {
            return Ok(());
        }
        let parent = self.parents(name, raw)?;
        let key = name.folded();
        match self.nodes.get_mut(&key) {
            Some(Node::Dir { listed, .. }) if !*listed => {
                *listed = true;
                Ok(())
            }
            Some(_) => Err(ExtractError::refused(Refusal::Duplicate, raw)),
            None => {
                let rel = join(&parent, last(name));
                let p = self.at(&rel);
                create_dir(&p).map_err(|e| self.create_failed(e, raw))?;
                self.nodes.insert(key, Node::Dir { listed: true, rel });
                self.dirs.push(p);
                Ok(())
            }
        }
    }

    /// Creates the file `name` and copies `body` into it, checking every
    /// chunk against the caps. `compressed` reports, at each call, how
    /// many compressed bytes this entry and the whole archive have used.
    fn file(
        &mut self,
        name: &Normalised,
        raw: &[u8],
        claimed: u64,
        body: &mut dyn Read,
        tripped: Option<&Tripped>,
        compressed: &dyn Fn() -> (u64, u64),
    ) -> Result<(), ExtractError> {
        if name.components.is_empty() || name.trailing_slash {
            return Err(ExtractError::refused(Refusal::EmptyComponent, raw));
        }
        let parent = self.parents(name, raw)?;
        let key = name.folded();
        if self.nodes.contains_key(&key) {
            return Err(ExtractError::refused(Refusal::Duplicate, raw));
        }
        let rel = join(&parent, last(name));
        let path = self.at(&rel);
        let mut file = create_file(&path).map_err(|e| self.create_failed(e, raw))?;
        self.nodes.insert(key, Node::File);
        self.files += 1;

        let mut written = 0u64;
        let mut buf = vec![0u8; COPY_BUF];
        loop {
            let n = match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    return Err(match tripped {
                        Some(t) => stream_failed(t, Some(raw)),
                        None => ExtractError::Corrupt,
                    });
                }
            };
            let n64 = n as u64;
            written += n64;
            self.total += n64;
            self.body.set(self.total);
            if self.total > self.limits.total {
                return Err(ExtractError::refused(Refusal::TooLarge, raw));
            }
            let (entry_in, archive_in) = compressed();
            if !self.limits.ratio_ok(written, entry_in)
                || !self.limits.ratio_ok(self.total, archive_in)
            {
                return Err(ExtractError::refused(Refusal::Ratio, raw));
            }
            file.write_all(&buf[..n]).map_err(io_err)?;
        }
        if written != claimed {
            return Err(ExtractError::refused(Refusal::SizeMismatch, raw));
        }
        let executable = rel == self.executable.as_str();
        self.executable_written |= executable;
        set_mode(&file, if executable { 0o755 } else { 0o644 }).map_err(io_err)?;
        file.sync_all().map_err(io_err)?;
        Ok(())
    }

    /// Checks the executable and fsyncs the directories, deepest first.
    fn finish(self) -> Result<Extracted, ExtractError> {
        let shown = self.executable.as_str().as_bytes();
        if !self.executable_written {
            let folded = self.executable.as_str().to_ascii_lowercase();
            let refusal = match self.nodes.get(&folded) {
                Some(Node::Dir { .. }) => Refusal::ExecutableNotFile,
                // Absent, or present in another case: on a case-sensitive
                // filesystem that's another file.
                _ => Refusal::MissingExecutable,
            };
            return Err(ExtractError::refused(refusal, shown));
        }
        for d in self.dirs.iter().rev() {
            set_dir_mode(d).map_err(io_err)?;
            sync_dir(d).map_err(io_err)?;
        }
        set_dir_mode(self.root).map_err(io_err)?;
        sync_dir(self.root).map_err(io_err)?;
        Ok(Extracted {
            entries: self.entries,
            files: self.files,
            bytes: self.total,
        })
    }
}

fn join(parent: &str, c: &str) -> String {
    if parent.is_empty() {
        c.to_owned()
    } else {
        format!("{parent}/{c}")
    }
}

fn last(name: &Normalised) -> &str {
    name.components.last().map_or("", String::as_str)
}

fn create_dir(path: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o755);
    }
    b.create(path)
}

/// A new file, never one that exists and never through a symlink: with
/// `create_new`, a symlink in its place (even a dangling one) is an
/// `AlreadyExists` error (`O_CREAT|O_EXCL` on Unix, `CREATE_NEW` on
/// Windows).
fn create_file(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// Sets a file's mode through its handle, so the umask doesn't change it.
fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (file, mode);
        Ok(())
    }
}

/// Sets a directory this extraction created to 0755, whatever the umask.
fn set_dir_mode(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Fsyncs a directory (Unix). Windows can't open one with `std`, and its
/// renames are durable with the files' own flushes.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

// ─── Reading ────────────────────────────────────────────────────────────

/// Counts the compressed bytes read from the archive, and passes seeks
/// through (zip's index is at the end).
/// While `budget` holds a number, reading past it fails the read and sets
/// `tripped`: the zip crate allocates for every index record it reads
/// while opening, so the bytes it may read then are capped, whatever its
/// rules for finding the index.
struct Counting<'a> {
    inner: &'a mut dyn Archive,
    read: Rc<Cell<u64>>,
    budget: Rc<Cell<Option<u64>>>,
    tripped: Tripped,
}

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get().saturating_add(n as u64));
        if self.budget.get().is_some_and(|b| self.read.get() > b) {
            self.tripped.set(Some(Refusal::TooLarge));
            return Err(io::Error::other("archive index cap"));
        }
        Ok(n)
    }
}

impl Seek for Counting<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Why a stream stopped: set by [`Guard`] before it fails a read, so the
/// error the parser returns can be reported as the refusal it was.
type Tripped = Rc<Cell<Option<Refusal>>>;

/// Bounds everything a tar parser decompresses, not only the file bodies
/// [`Out::file`] sees: headers, GNU long names, pax records and the data
/// of entries it skips. The tar parser reads a long name or pax record
/// whole into memory, so what isn't a file body is capped on its own at
/// [`Limits::overhead`]; a ratio over the cap against the compressed
/// bytes read stops it too. Without it, one long-name header claiming
/// gigabytes would be read into memory before any cap ran.
struct Guard<R> {
    inner: R,
    limits: Limits,
    inflated: u64,
    read: Rc<Cell<u64>>,
    /// File bytes written so far ([`Out::body`]).
    body: Rc<Cell<u64>>,
    tripped: Tripped,
}

impl<R: Read> Read for Guard<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.inflated = self.inflated.saturating_add(n as u64);
        // The read being returned is at most one copy buffer ahead of the
        // body count, which is part of the allowance.
        let overhead = self.inflated.saturating_sub(self.body.get());
        let refusal = if overhead > self.limits.overhead.saturating_add(COPY_BUF as u64)
            || self.inflated > self.limits.total.saturating_add(self.limits.overhead)
        {
            Some(Refusal::TooLarge)
        } else if !self.limits.ratio_ok(self.inflated, self.read.get()) {
            Some(Refusal::Ratio)
        } else {
            None
        };
        if let Some(r) = refusal {
            self.tripped.set(Some(r));
            return Err(io::Error::other("archive cap"));
        }
        Ok(n)
    }
}

/// The error for a stream that failed: the refusal [`Guard`] tripped on,
/// or corrupt.
fn stream_failed(tripped: &Tripped, raw: Option<&[u8]>) -> ExtractError {
    match (tripped.get(), raw) {
        (Some(r), Some(raw)) => ExtractError::refused(r, raw),
        (Some(refusal), None) => ExtractError::Refused {
            refusal,
            entry: None,
        },
        (None, _) => ExtractError::Corrupt,
    }
}

// ─── Zip ────────────────────────────────────────────────────────────────

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;
const SPECIAL_BITS: u32 = 0o7000;

/// The zip reader's buffer (`std`'s default), which may run ahead of the
/// entry being decoded.
const ZIP_READ_AHEAD: u64 = 8 * 1024;

fn zip_entries(archive: &mut dyn Archive, len: u64, out: &mut Out<'_>) -> Result<(), ExtractError> {
    let read = Rc::new(Cell::new(0u64));
    let budget = Rc::new(Cell::new(Some(out.limits.overhead)));
    let tripped: Tripped = Rc::new(Cell::new(None));
    let counting = Counting {
        inner: archive,
        read: read.clone(),
        budget: budget.clone(),
        tripped: tripped.clone(),
    };
    // Opening reads the index, allocating as it goes: capped at the
    // overhead budget (a 20,000-entry index is a few megabytes).
    let mut zip = zip::ZipArchive::new(counting).map_err(|_| stream_failed(&tripped, None))?;
    budget.set(None);
    if zip.len() > out.limits.entries {
        return Err(ExtractError::Refused {
            refusal: Refusal::TooManyEntries,
            entry: None,
        });
    }
    // Duplicates the zip crate would refuse anyway; checked first so it's
    // a refusal, not "corrupt".
    if zip.has_duplicate_names() {
        return Err(ExtractError::Refused {
            refusal: Refusal::Duplicate,
            entry: None,
        });
    }
    // Defence in depth: every local header matches the index, none is
    // missing, and no two entries overlap.
    zip.validate_header_consistency()
        .map_err(|_| ExtractError::Corrupt)?;
    for i in 0..zip.len() {
        // The raw view first: `by_index` fails an encrypted entry before
        // it could be named.
        let encrypted = zip
            .by_index_raw(i)
            .map_err(|_| ExtractError::Corrupt)?
            .encrypted();
        let start = read.get();
        let mut entry = zip.by_index(i).map_err(|_| {
            if encrypted {
                ExtractError::Refused {
                    refusal: Refusal::Encrypted,
                    entry: None,
                }
            } else {
                ExtractError::Corrupt
            }
        })?;
        let raw = entry.name_raw().to_vec();
        out.count(&raw)?;
        let name = normalise(&raw).map_err(|r| ExtractError::refused(r, &raw))?;
        if encrypted {
            return Err(ExtractError::refused(Refusal::Encrypted, &raw));
        }
        // The zip crate's `unix_mode` clears the setuid, setgid and sticky
        // bits and ignores the Unix half of a DOS-made entry's attributes,
        // so the raw attributes are read: any non-zero upper half is a
        // Unix mode, whatever system the entry says made it.
        let mode = Some(entry.external_attributes() >> 16).filter(|&m| m != 0);
        if let Some(m) = mode {
            match m & S_IFMT {
                0 | S_IFREG | S_IFDIR => {}
                S_IFLNK => return Err(ExtractError::refused(Refusal::Link, &raw)),
                _ => return Err(ExtractError::refused(Refusal::Special, &raw)),
            }
            if m & SPECIAL_BITS != 0 {
                return Err(ExtractError::refused(Refusal::ModeBits, &raw));
            }
        }
        let typed_dir = mode.is_some_and(|m| m & S_IFMT == S_IFDIR);
        if name.trailing_slash || typed_dir {
            if !name.trailing_slash || entry.size() != 0 {
                return Err(ExtractError::refused(Refusal::Special, &raw));
            }
            out.dir(&name, &raw)?;
            continue;
        }
        if entry.compressed_size() > len {
            return Err(ExtractError::Corrupt);
        }
        let claimed = entry.size();
        // The entry's denominator is what was really read for it (its
        // header's compressed size can be inflated), plus the reader's
        // read-ahead.
        let counter = read.clone();
        out.file(&name, &raw, claimed, &mut entry, None, &move || {
            let used = counter.get().saturating_sub(start);
            (used.saturating_add(ZIP_READ_AHEAD), len)
        })?;
    }
    Ok(())
}

// ─── Tar.gz ─────────────────────────────────────────────────────────────

fn tar_gz_entries(archive: &mut dyn Archive, out: &mut Out<'_>) -> Result<(), ExtractError> {
    use tar::EntryType;

    let read = Rc::new(Cell::new(0u64));
    let tripped: Tripped = Rc::new(Cell::new(None));
    let counting = Counting {
        inner: archive,
        read: read.clone(),
        budget: Rc::new(Cell::new(None)),
        tripped: tripped.clone(),
    };
    let gz = flate2::bufread::GzDecoder::new(BufReader::with_capacity(READ_AHEAD, counting));
    let guard = Guard {
        inner: gz,
        limits: out.limits,
        inflated: 0,
        read: read.clone(),
        body: out.body.clone(),
        tripped: tripped.clone(),
    };
    let mut tar = tar::Archive::new(guard);
    let entries = tar.entries().map_err(|_| stream_failed(&tripped, None))?;
    for entry in entries {
        let mut entry = entry.map_err(|_| stream_failed(&tripped, None))?;
        let raw = entry.path_bytes().into_owned();
        out.count(&raw)?;
        let kind = entry.header().entry_type();
        if kind == EntryType::XGlobalHeader {
            // pax global headers (`git archive` writes one with the commit)
            // carry no file. Their records are ignored, as tar does here;
            // their bodies are bounded by the guard.
            continue;
        }
        let name = normalise(&raw).map_err(|r| ExtractError::refused(r, &raw))?;
        match kind {
            EntryType::Regular | EntryType::Directory => {}
            EntryType::Symlink | EntryType::Link => {
                return Err(ExtractError::refused(Refusal::Link, &raw));
            }
            _ => return Err(ExtractError::refused(Refusal::Special, &raw)),
        }
        let mode = entry.header().mode().map_err(|_| ExtractError::Corrupt)?;
        if mode & SPECIAL_BITS != 0 {
            return Err(ExtractError::refused(Refusal::ModeBits, &raw));
        }
        let claimed = entry.size();
        if kind == EntryType::Directory {
            if claimed != 0 {
                return Err(ExtractError::refused(Refusal::Special, &raw));
            }
            out.dir(&name, &raw)?;
            continue;
        }
        let start = read.get();
        let counter = read.clone();
        out.file(
            &name,
            &raw,
            claimed,
            &mut entry,
            Some(&tripped),
            &move || {
                let now = counter.get();
                (now - start + READ_AHEAD as u64, now)
            },
        )?;
    }
    let guard = tar.into_inner();
    tar_trailer(guard, &tripped)
}

/// Reads what's left of the gzip stream after the tar's end, so its CRC
/// and length are checked: a corrupt or truncated stream is refused even
/// when the damage is past the last entry.
fn tar_trailer<R: Read>(guard: R, tripped: &Tripped) -> Result<(), ExtractError> {
    let mut rest = guard.take(MAX_TRAILER + 1);
    let n = io::copy(&mut rest, &mut io::sink()).map_err(|_| stream_failed(tripped, None))?;
    if n > MAX_TRAILER {
        return Err(ExtractError::Refused {
            refusal: Refusal::TooLarge,
            entry: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Result<Vec<String>, Refusal> {
        normalise(s.as_bytes()).map(|x| x.components)
    }

    #[test]
    fn accepts_ordinary_names() {
        assert_eq!(n("Prowlarr/Prowlarr").unwrap(), ["Prowlarr", "Prowlarr"]);
        assert_eq!(n("./bin/x.dll").unwrap(), ["bin", "x.dll"]);
        assert_eq!(n("a b/c-d_e.f~1").unwrap(), ["a b", "c-d_e.f~1"]);
        assert!(normalise(b"dir/").unwrap().trailing_slash);
        assert!(n("./").unwrap().is_empty());
        assert!(n(".").unwrap().is_empty());
    }

    #[test]
    fn refuses_each_bad_name() {
        let cases: &[(&[u8], Refusal)] = &[
            (b"", Refusal::EmptyComponent),
            (b"/etc/passwd", Refusal::Absolute),
            (b"//server/share/x", Refusal::Absolute),
            (b"C:/x", Refusal::Absolute),
            (b"c:x", Refusal::Absolute),
            (b"..\\x", Refusal::Backslash),
            (b"a\\b", Refusal::Backslash),
            (b"../x", Refusal::Traversal),
            (b"a/../../x", Refusal::Traversal),
            (b"a/..", Refusal::Traversal),
            (b"a//b", Refusal::EmptyComponent),
            (b"a/./b", Refusal::DotComponent),
            (b"././a", Refusal::DotComponent),
            (b"ab:stream", Refusal::Colon),
            (b"a/b:$DATA", Refusal::Colon),
            (b"CON", Refusal::ReservedName),
            (b"bin/nul.txt", Refusal::ReservedName),
            (b"com1", Refusal::ReservedName),
            (b"LPT9.log", Refusal::ReservedName),
            (b"nul .txt", Refusal::ReservedName),
            (b"conout$", Refusal::ReservedName),
            (b"dir./x", Refusal::TrailingDotOrSpace),
            (b"x ", Refusal::TrailingDotOrSpace),
            (b"a\0b", Refusal::Character),
            (b"a\nb", Refusal::Character),
            (b"caf\xc3\xa9", Refusal::Character),
            (b"a*b", Refusal::Character),
            (b"a?b", Refusal::Character),
            (b"a<b", Refusal::Character),
            (b"a|b", Refusal::Character),
            (b"a\"b", Refusal::Character),
        ];
        for (raw, want) in cases {
            assert_eq!(normalise(raw), Err(*want), "{:?}", EntryName::new(raw));
        }
        assert_eq!(n(&"a".repeat(256)), Err(Refusal::NameTooLong));
        assert_eq!(n(&vec!["a"; 65].join("/")), Err(Refusal::NameTooLong));
        assert_eq!(n(&"a/".repeat(600)), Err(Refusal::NameTooLong));
    }

    #[test]
    fn names_in_errors_are_bounded_and_printable() {
        let shown = EntryName::new(&[b'\x1b'; 500]);
        assert!(shown.as_str().chars().count() <= SHOWN_NAME + 1);
        assert!(shown.as_str().starts_with("???"));
        let e = ExtractError::refused(Refusal::Link, b"a\x1b[31mred");
        assert_eq!(e.to_string(), "archive refused (link) at \"a?[31mred\"");
    }

    #[test]
    fn ratio_has_a_floor_then_holds() {
        let l = Limits::Q16;
        assert!(l.ratio_ok(RATIO_FLOOR, 0));
        assert!(!l.ratio_ok(RATIO_FLOOR + 1, 0));
        assert!(l.ratio_ok(100_000_000, 1_000_000));
        assert!(!l.ratio_ok(100_000_001, 1_000_000));
    }
}
