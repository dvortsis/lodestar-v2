//! Where a managed companion lives on disk, and how a version is switched
//! (plan 2026-11-06-v06-companions §4 CMP-2; architecture §18.10).
//!
//! ```text
//! <data>/companions/<id>/
//!   current              which version runs, and the one kept for rollback
//!   <version>/           one extracted archive per version
//!   <version>.staging/   an extraction in progress (never run)
//! ```
//!
//! **The pointer** is the small JSON file `current`
//! (`{"format":1,"current":"5.0.3","previous":"5.0.2"}`), written by a
//! new temporary file, fsync, rename over `current` and an fsync of the
//! folder. A rename over a file is atomic on Windows as on Unix, so no
//! symlink is needed and a crash leaves either the old pointer or the new
//! one. The state row that mirrors it is CMP-5's.
//!
//! **The steps CMP-5 calls,** each one safe to stop after:
//! 1. [`Install::stage`] extracts into `<version>.staging/`
//!    ([`crate::extract`]) and renames it to `<version>/` once it's
//!    complete and fsynced. The pointer doesn't change.
//! 2. [`Install::commit`] points `current` at it, keeping the version it
//!    replaces as `previous`.
//! 3. After the new version passes health, [`Install::prune_previous`]
//!    deletes the previous one; if it fails, [`Install::rollback`] points
//!    back to it and deletes the new one.
//!
//! [`Install::sweep`] finishes whatever a crash interrupted: it deletes
//! version and staging folders the pointer doesn't name, and a leftover
//! temporary pointer. Nothing else in the folder is touched, so other
//! slices' files there are safe. [`Install::remove`] deletes the whole
//! folder.
//!
//! **Deleting stays inside `<data>/companions/<id>/`.** The folder and
//! `<data>/companions/` must be real directories, not links, and the
//! folder's canonical path must be the canonical `<data>` joined with
//! `companions/<id>`; that's checked when it's opened and again before
//! anything is deleted. Deletion uses `std::fs::remove_dir_all`, which
//! never follows a symlink or a junction: one found inside is removed as
//! a link, and its target is left alone.
//!
//! **One caller per companion.** Every step that changes the folder takes
//! `&mut self`, and CMP-5 holds one [`Install`] per companion.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalogue::{ArchiveKind, RelPath};
use crate::extract::{self, Archive, ExtractError, Extracted, sync_dir};
use crate::inputs::{CompanionId, Version};

/// The pointer's file name.
pub const POINTER: &str = "current";
const POINTER_TMP: &str = "current.tmp";
const STAGING: &str = ".staging";
const POINTER_FORMAT: u32 = 1;
const MAX_POINTER: u64 = 4096;

/// Which version runs, and the one kept for rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    pub current: Version,
    pub previous: Option<Version>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PointerFile {
    format: u32,
    current: String,
    #[serde(default)]
    previous: Option<String>,
}

/// Why a layout step failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum LayoutError {
    /// Extraction refused or failed; nothing was renamed into place.
    Extract(ExtractError),
    /// A path that must be a real directory is a link, a file or
    /// something else, or the folder resolves outside
    /// `<data>/companions/<id>/`. Nothing was deleted.
    OutsideRoot,
    /// `<version>/` is already there. Stage another version, or
    /// [`Install::sweep`] if the pointer doesn't name it.
    AlreadyStaged,
    /// The version to commit hasn't been staged.
    NotStaged,
    /// Rollback or pruning with no previous version.
    NoPrevious,
    /// No version has been committed.
    NoCurrent,
    /// `current` can't be read as a pointer. It's left as it is.
    BadPointer,
    /// The disk failed.
    Io(io::ErrorKind),
}

impl LayoutError {
    /// A stable code for logs and the API.
    pub fn code(&self) -> &'static str {
        match self {
            LayoutError::Extract(e) => e.code(),
            LayoutError::OutsideRoot => "outside_root",
            LayoutError::AlreadyStaged => "already_staged",
            LayoutError::NotStaged => "not_staged",
            LayoutError::NoPrevious => "no_previous",
            LayoutError::NoCurrent => "no_current",
            LayoutError::BadPointer => "bad_pointer",
            LayoutError::Io(_) => "io",
        }
    }
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::Extract(e) => e.fmt(f),
            LayoutError::Io(k) => write!(f, "companion folder: {k}"),
            other => write!(f, "companion folder: {}", other.code()),
        }
    }
}

impl std::error::Error for LayoutError {}

impl From<io::Error> for LayoutError {
    fn from(e: io::Error) -> LayoutError {
        LayoutError::Io(e.kind())
    }
}

/// One companion's folder, `<data>/companions/<id>/`.
#[derive(Debug)]
pub struct Install {
    /// The canonical folder.
    root: PathBuf,
}

impl Install {
    /// Opens the folder, creating it and `<data>/companions/` (0755) if
    /// they're missing. `data_dir` must exist.
    pub fn open(data_dir: &Path, id: &CompanionId) -> Result<Install, LayoutError> {
        let data = fs::canonicalize(data_dir)?;
        let companions = data.join("companions");
        ensure_dir(&companions)?;
        let root = companions.join(id.as_str());
        ensure_dir(&root)?;
        Install::check(&data, id).map(|root| Install { root })
    }

    /// The folder if it exists; `None` if it doesn't. Nothing is created.
    pub fn existing(data_dir: &Path, id: &CompanionId) -> Result<Option<Install>, LayoutError> {
        let data = fs::canonicalize(data_dir)?;
        match fs::symlink_metadata(data.join("companions").join(id.as_str())) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
            Ok(_) => Install::check(&data, id).map(|root| Some(Install { root })),
        }
    }

    /// The folder's checked path: `companions` and `<id>` are real
    /// directories, and the whole resolves to where it should.
    fn check(data: &Path, id: &CompanionId) -> Result<PathBuf, LayoutError> {
        let companions = data.join("companions");
        let root = companions.join(id.as_str());
        for p in [&companions, &root] {
            if !real_dir(p)? {
                return Err(LayoutError::OutsideRoot);
            }
        }
        if fs::canonicalize(&root)? != root {
            return Err(LayoutError::OutsideRoot);
        }
        Ok(root)
    }

    /// Re-checks the folder before anything under it is deleted.
    fn recheck(&self) -> Result<(), LayoutError> {
        if !real_dir(&self.root)? || fs::canonicalize(&self.root)? != self.root {
            return Err(LayoutError::OutsideRoot);
        }
        Ok(())
    }

    /// The folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where `version` is (or would be) extracted.
    pub fn version_dir(&self, version: &Version) -> PathBuf {
        self.root.join(version.as_str())
    }

    /// The catalogue's executable inside `version`.
    pub fn executable(&self, version: &Version, executable: &RelPath) -> PathBuf {
        let mut p = self.version_dir(version);
        p.extend(executable.components());
        p
    }

    fn staging_dir(&self, version: &Version) -> PathBuf {
        self.root.join(format!("{}{STAGING}", version.as_str()))
    }

    /// Step 1: extracts `archive` and renames it to `<version>/`. A stale
    /// `<version>.staging/` from a crash is removed first, if it's a plain
    /// directory; anything else in its place refuses. The pointer is
    /// unchanged.
    pub fn stage(
        &mut self,
        version: &Version,
        kind: ArchiveKind,
        archive: &mut dyn Archive,
        len: u64,
        executable: &RelPath,
    ) -> Result<Extracted, LayoutError> {
        let target = self.version_dir(version);
        if exists(&target)? {
            return Err(LayoutError::AlreadyStaged);
        }
        let staging = self.staging_dir(version);
        if exists(&staging)? {
            if !real_dir(&staging)? {
                return Err(LayoutError::Extract(ExtractError::StagingInTheWay));
            }
            self.delete(&staging)?;
        }
        let done = extract::extract(kind, archive, len, executable, &staging)
            .map_err(LayoutError::Extract)?;
        if let Err(e) = fs::rename(&staging, &target) {
            // Best effort: the next stage or sweep removes it otherwise.
            let _ = self.delete(&staging);
            return Err(e.into());
        }
        sync_dir(&self.root)?;
        Ok(done)
    }

    /// The pointer, or `None` before the first commit.
    pub fn pointer(&self) -> Result<Option<Pointer>, LayoutError> {
        let path = self.root.join(POINTER);
        let meta = match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other?,
        };
        if !meta.is_file() || meta.len() > MAX_POINTER {
            return Err(LayoutError::BadPointer);
        }
        let mut text = String::new();
        File::open(&path)?
            .take(MAX_POINTER)
            .read_to_string(&mut text)
            .map_err(|_| LayoutError::BadPointer)?;
        let file: PointerFile = serde_json::from_str(&text).map_err(|_| LayoutError::BadPointer)?;
        let version = |s: &str| {
            Version::parse(s)
                .ok()
                .filter(|v| v.as_str() == s)
                .ok_or(LayoutError::BadPointer)
        };
        if file.format != POINTER_FORMAT {
            return Err(LayoutError::BadPointer);
        }
        Ok(Some(Pointer {
            current: version(&file.current)?,
            previous: file.previous.as_deref().map(version).transpose()?,
        }))
    }

    /// Step 2: points `current` at the staged `version`, keeping the one
    /// it replaces as `previous`. A version two commits old that nothing
    /// names any more is deleted afterwards.
    pub fn commit(&mut self, version: &Version) -> Result<Pointer, LayoutError> {
        if !real_dir(&self.version_dir(version)).unwrap_or(false) {
            return Err(LayoutError::NotStaged);
        }
        let old = self.pointer()?;
        let previous = match old {
            Some(p) if &p.current != version => Some(p.current),
            Some(p) => p.previous,
            None => None,
        };
        let new = Pointer {
            current: version.clone(),
            previous,
        };
        self.write_pointer(&new)?;
        self.sweep()?;
        Ok(new)
    }

    /// Points back to the previous version and deletes the one rolled
    /// back from.
    pub fn rollback(&mut self) -> Result<Pointer, LayoutError> {
        let old = self.pointer()?.ok_or(LayoutError::NoCurrent)?;
        let previous = old.previous.ok_or(LayoutError::NoPrevious)?;
        if !real_dir(&self.version_dir(&previous)).unwrap_or(false) {
            return Err(LayoutError::NotStaged);
        }
        let new = Pointer {
            current: previous,
            previous: None,
        };
        self.write_pointer(&new)?;
        self.sweep()?;
        Ok(new)
    }

    /// Step 3, once the new version is healthy: deletes the previous one.
    pub fn prune_previous(&mut self) -> Result<Pointer, LayoutError> {
        let old = self.pointer()?.ok_or(LayoutError::NoCurrent)?;
        if old.previous.is_none() {
            return Err(LayoutError::NoPrevious);
        }
        let new = Pointer {
            current: old.current,
            previous: None,
        };
        self.write_pointer(&new)?;
        self.sweep()?;
        Ok(new)
    }

    /// Deletes every `<version>/` and `<version>.staging/` the pointer
    /// doesn't name, and a leftover temporary pointer: what a crash
    /// between the steps leaves. Returns the names it deleted. Run by
    /// CMP-5 at start, before any step.
    pub fn sweep(&mut self) -> Result<Vec<String>, LayoutError> {
        let pointer = self.pointer()?;
        let keep = |v: &Version| {
            pointer
                .as_ref()
                .is_some_and(|p| &p.current == v || p.previous.as_ref() == Some(v))
        };
        let mut gone = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let ours = if name == POINTER_TMP {
                true
            } else {
                let (stem, staging) = match name.strip_suffix(STAGING) {
                    Some(stem) => (stem, true),
                    None => (name.as_str(), false),
                };
                match Version::parse(stem) {
                    Ok(v) if v.as_str() == stem => staging || !keep(&v),
                    _ => false,
                }
            };
            if ours {
                self.delete(&entry.path())?;
                gone.push(name);
            }
        }
        if !gone.is_empty() {
            sync_dir(&self.root)?;
        }
        gone.sort();
        Ok(gone)
    }

    /// Deletes the whole folder, for an uninstall. The folder is checked
    /// again first; a link inside is removed as a link.
    pub fn remove(self) -> Result<(), LayoutError> {
        self.recheck()?;
        fs::remove_dir_all(&self.root)?;
        if let Some(parent) = self.root.parent() {
            sync_dir(parent)?;
        }
        Ok(())
    }

    /// Deletes one direct child of the folder without following a link:
    /// a directory with `remove_dir_all`, anything else (a link included)
    /// as a file.
    fn delete(&self, path: &Path) -> Result<(), LayoutError> {
        if path.parent() != Some(self.root.as_path()) {
            return Err(LayoutError::OutsideRoot);
        }
        self.recheck()?;
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            remove_link_or_file(path)?;
        }
        Ok(())
    }

    fn write_pointer(&self, p: &Pointer) -> Result<(), LayoutError> {
        let bytes = serde_json::to_vec(&PointerFile {
            format: POINTER_FORMAT,
            current: p.current.as_str().to_owned(),
            previous: p.previous.as_ref().map(|v| v.as_str().to_owned()),
        })
        .map_err(|_| LayoutError::BadPointer)?;
        let tmp = self.root.join(POINTER_TMP);
        if exists(&tmp)? {
            self.delete(&tmp)?;
        }
        let mut o = OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o644);
        }
        let mut f = o.open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, self.root.join(POINTER))?;
        sync_dir(&self.root)?;
        Ok(())
    }
}

/// Whether anything is at `path`, without following a link there.
fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether `path` is a directory itself, not a link to one.
fn real_dir(path: &Path) -> io::Result<bool> {
    let ft = fs::symlink_metadata(path)?.file_type();
    Ok(ft.is_dir() && !ft.is_symlink())
}

/// Creates `path` (0755) unless it's there already.
fn ensure_dir(path: &Path) -> Result<(), LayoutError> {
    let mut b = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o755);
    }
    match b.create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Removes a file or a link. On Windows a directory symlink or junction
/// is removed with `remove_dir`, which removes the link, not its target.
fn remove_link_or_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(_) => fs::remove_dir(path),
        #[cfg(not(windows))]
        Err(e) => Err(e),
    }
}
