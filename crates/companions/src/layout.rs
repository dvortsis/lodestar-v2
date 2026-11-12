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
//!    replaces as `previous`. The folder must hold the catalogue's
//!    executable as a regular file.
//! 3. After the new version passes health, [`Install::prune_previous`]
//!    deletes the previous one; if it fails, [`Install::rollback`] points
//!    back to it and deletes the new one.
//!
//! Steps 2 and 3 succeed once the pointer is written; a failure to delete
//! what it no longer names comes back in [`Switched::leftover`].
//! Versions are compared by their text (`1` and `1.0` are different
//! folders). A folder is deleted by renaming it to `<name>.deleting`
//! first, so a crash never leaves a partial `<version>/`.
//!
//! [`Install::sweep`] finishes whatever a crash interrupted: it deletes
//! version and staging folders the pointer doesn't name, tombstones and a
//! leftover temporary pointer. Nothing else in the folder is touched, so other
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
/// A folder being deleted is renamed to `<name>.deleting` first, so a
/// crash partway through leaves a name only [`Install::sweep`] wants,
/// never a partial `<version>/` that looks staged.
const DELETING: &str = ".deleting";
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

/// What a switch (commit, rollback, prune) did. Once the pointer is
/// renamed into place the switch has happened, so it's returned as a
/// success even if the folder's fsync or deleting what's no longer named
/// then fails (on Windows, a running
/// program's files can't be deleted): `leftover` holds that error, and a
/// later [`Install::sweep`] finishes the job.
#[derive(Debug)]
pub struct Switched {
    pub pointer: Pointer,
    pub leftover: Option<LayoutError>,
}

/// Versions compare by their text: `1` and `1.0` are different folders.
fn same(a: &Version, b: &Version) -> bool {
    a.as_str() == b.as_str()
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
        // Checked before `<id>` is created, so a linked `companions` never
        // gets a folder made inside its target.
        if !real_dir(&companions)? || fs::canonicalize(&companions)? != companions {
            return Err(LayoutError::OutsideRoot);
        }
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
    ///
    /// An `Io` error after the rename (the folder's fsync) leaves
    /// `<version>/` in place: a retry gets [`LayoutError::AlreadyStaged`],
    /// and the caller commits it or [`Install::sweep`]s it.
    pub fn stage(
        &mut self,
        version: &Version,
        kind: ArchiveKind,
        archive: &mut dyn Archive,
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
        let done =
            extract::extract(kind, archive, executable, &staging).map_err(LayoutError::Extract)?;
        if let Err(e) = fs::rename(&staging, &target) {
            // Best effort: the next stage or sweep removes it otherwise.
            let _ = self.delete(&staging);
            return Err(e.into());
        }
        sync_dir(&self.root)?;
        Ok(done)
    }

    /// Step 1 for a bare executable ([`ArchiveKind::Binary`]): copies
    /// the verified download into `<version>.staging/<name>` (0700 on
    /// Unix, created new, at most `max` bytes, fsynced) and renames the
    /// folder to `<version>/`, as [`Install::stage`] does. `name` is one
    /// component. The pointer is unchanged.
    pub fn stage_binary(
        &mut self,
        version: &Version,
        file: &mut dyn Read,
        name: &RelPath,
        max: u64,
    ) -> Result<u64, LayoutError> {
        if name.components().count() != 1 {
            return Err(LayoutError::Extract(ExtractError::StagingInTheWay));
        }
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
        let copied = (|| -> Result<u64, LayoutError> {
            fs::create_dir(&staging)?;
            let mut o = OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o700);
            }
            let mut out = o.open(staging.join(name.as_str()))?;
            let n = io::copy(&mut file.take(max.saturating_add(1)), &mut out)?;
            if n > max {
                return Err(LayoutError::Io(io::ErrorKind::InvalidData));
            }
            out.sync_all()?;
            // Closed before the rename: Windows refuses to rename a folder
            // while a file inside it is open.
            drop(out);
            sync_dir(&staging)?;
            fs::rename(&staging, &target)?;
            Ok(n)
        })();
        match copied {
            Ok(n) => {
                sync_dir(&self.root)?;
                Ok(n)
            }
            Err(e) => {
                // Best effort: the next stage or sweep removes it otherwise.
                let _ = self.delete(&staging);
                Err(e)
            }
        }
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

    /// Whether `version` is staged: a real `<version>/` folder holding
    /// the catalogue's `executable` as a regular file. An I/O error other
    /// than "not found" is returned, not read as "not staged".
    fn staged(&self, version: &Version, executable: Option<&RelPath>) -> Result<bool, LayoutError> {
        let not_found = |e: &io::Error| e.kind() == io::ErrorKind::NotFound;
        match real_dir(&self.version_dir(version)) {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(e) if not_found(&e) => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        let Some(x) = executable else {
            return Ok(true);
        };
        match fs::symlink_metadata(self.executable(version, x)) {
            Ok(m) => Ok(m.is_file()),
            Err(e) if not_found(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes `new`, then deletes what it no longer names.
    fn switch(&mut self, new: Pointer) -> Result<Switched, LayoutError> {
        let unsynced = self.write_pointer(&new)?;
        Ok(self.after_switch(new, unsynced))
    }

    /// The cleanup after the pointer's rename. If the folder's fsync
    /// failed, the rename may not survive a crash, and the old pointer
    /// could come back naming what a sweep would delete now: so nothing
    /// is swept, and the fsync error is the leftover. A later sweep, on a
    /// pointer that's durable, finishes the job.
    fn after_switch(&mut self, new: Pointer, unsynced: Option<LayoutError>) -> Switched {
        let leftover = match unsynced {
            Some(e) => Some(e),
            None => self.sweep().err(),
        };
        Switched {
            pointer: new,
            leftover,
        }
    }

    /// Step 2: points `current` at the staged `version`, keeping the one
    /// it replaces as `previous`. `version` must hold the catalogue's
    /// `executable` as a regular file. A version two commits old that
    /// nothing names any more is deleted afterwards ([`Switched`]).
    pub fn commit(
        &mut self,
        version: &Version,
        executable: &RelPath,
    ) -> Result<Switched, LayoutError> {
        if !self.staged(version, Some(executable))? {
            return Err(LayoutError::NotStaged);
        }
        let old = self.pointer()?;
        let previous = match old {
            Some(p) if !same(&p.current, version) => Some(p.current),
            Some(p) => p.previous,
            None => None,
        };
        self.switch(Pointer {
            current: version.clone(),
            previous,
        })
    }

    /// Points back to the previous version and deletes the one rolled
    /// back from ([`Switched`]).
    pub fn rollback(&mut self) -> Result<Switched, LayoutError> {
        let old = self.pointer()?.ok_or(LayoutError::NoCurrent)?;
        let previous = old.previous.ok_or(LayoutError::NoPrevious)?;
        if !self.staged(&previous, None)? {
            return Err(LayoutError::NotStaged);
        }
        self.switch(Pointer {
            current: previous,
            previous: None,
        })
    }

    /// Step 3, once the new version is healthy: deletes the previous one
    /// ([`Switched`]).
    pub fn prune_previous(&mut self) -> Result<Switched, LayoutError> {
        let old = self.pointer()?.ok_or(LayoutError::NoCurrent)?;
        if old.previous.is_none() {
            return Err(LayoutError::NoPrevious);
        }
        self.switch(Pointer {
            current: old.current,
            previous: None,
        })
    }

    /// Deletes every `<version>/` and `<version>.staging/` the pointer
    /// doesn't name, every `*.deleting` folder and a leftover temporary
    /// pointer: what a crash between the steps leaves. Returns the names it deleted. Run by
    /// CMP-5 at start, before any step.
    pub fn sweep(&mut self) -> Result<Vec<String>, LayoutError> {
        let pointer = self.pointer()?;
        let keep = |v: &Version| {
            pointer.as_ref().is_some_and(|p| {
                same(&p.current, v) || p.previous.as_ref().is_some_and(|q| same(q, v))
            })
        };
        // Names first: deleting renames folders inside this one.
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            if let Some(name) = entry?.file_name().to_str() {
                names.push(name.to_owned());
            }
        }
        let mut gone = Vec::new();
        for name in names {
            let ours = if name == POINTER_TMP || name.ends_with(DELETING) {
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
                self.delete(&self.root.join(&name))?;
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
    ///
    /// The pointer goes first, fsynced, so a crash or a locked file
    /// partway through never leaves `current` naming a half-deleted
    /// version: what's left has no pointer, and [`Install::sweep`] takes
    /// every version folder. Each child then goes through [`Self::delete`]
    /// (folders by way of a `.deleting` tombstone), and the folder last.
    pub fn remove(self) -> Result<(), LayoutError> {
        self.remove_last(None)
    }

    /// [`Install::remove`], keeping the direct child named `last` until
    /// everything else is gone: a marker (CMP-5's journal) that says what
    /// the folder is for as long as anything else is left in it.
    pub fn remove_last(self, last: Option<&str>) -> Result<(), LayoutError> {
        self.remove_pointer()?;
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let name = entry?.file_name();
            if last.is_none_or(|l| name != l) {
                names.push(name);
            }
        }
        for name in names {
            self.delete(&self.root.join(name))?;
        }
        if let Some(l) = last {
            sync_dir(&self.root)?;
            self.delete(&self.root.join(l))?;
        }
        self.recheck()?;
        fs::remove_dir(&self.root)?;
        if let Some(parent) = self.root.parent() {
            sync_dir(parent)?;
        }
        Ok(())
    }

    /// Deletes only what this layout put in the folder (the pointer and
    /// its temporary file, version, staging and tombstone folders) plus
    /// the caller's own `extra` names, `last` after everything else, and
    /// then the folder only if nothing is left: anything a third party
    /// wrote there survives. Returns whether the folder is gone.
    pub fn remove_own(self, extra: &[&str], last: Option<&str>) -> Result<bool, LayoutError> {
        self.remove_pointer()?;
        let ours = |name: &str| {
            if extra.contains(&name) || name.ends_with(DELETING) {
                return true;
            }
            let stem = name.strip_suffix(STAGING).unwrap_or(name);
            Version::parse(stem).is_ok_and(|v| v.as_str() == stem)
        };
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let name = entry?.file_name();
            if let Some(name) = name.to_str().filter(|n| ours(n) && last != Some(*n)) {
                names.push(name.to_owned());
            }
        }
        for name in names {
            self.delete(&self.root.join(name))?;
        }
        if let Some(l) = last {
            sync_dir(&self.root)?;
            self.delete(&self.root.join(l))?;
        }
        self.recheck()?;
        // `remove_dir` fails on a folder that isn't empty: what someone
        // else put there keeps it.
        match fs::remove_dir(&self.root) {
            Ok(()) => {}
            Err(_) if fs::read_dir(&self.root).is_ok_and(|mut d| d.next().is_some()) => {
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }
        if let Some(parent) = self.root.parent() {
            sync_dir(parent)?;
        }
        Ok(true)
    }

    /// The first step of [`Install::remove`]: the pointer and any
    /// temporary pointer, then an fsync of the folder.
    fn remove_pointer(&self) -> Result<(), LayoutError> {
        self.recheck()?;
        self.delete(&self.root.join(POINTER))?;
        self.delete(&self.root.join(POINTER_TMP))?;
        sync_dir(&self.root)?;
        Ok(())
    }

    /// Deletes one direct child of the folder without following a link:
    /// a directory by renaming it to `<name>.deleting` and then
    /// `remove_dir_all`, anything else (a link included) as a file.
    /// Something already gone is fine.
    fn delete(&self, path: &Path) -> Result<(), LayoutError> {
        self.delete_steps(path, false)
    }

    /// [`Self::delete`], stopping after the rename when `stop_after_rename`
    /// (a crash there, for tests).
    fn delete_steps(&self, path: &Path, stop_after_rename: bool) -> Result<(), LayoutError> {
        if path.parent() != Some(self.root.as_path()) {
            return Err(LayoutError::OutsideRoot);
        }
        self.recheck()?;
        let meta = match fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            other => other?,
        };
        if !meta.is_dir() {
            remove_link_or_file(path)?;
            return Ok(());
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(LayoutError::OutsideRoot)?;
        if name.ends_with(DELETING) {
            fs::remove_dir_all(path)?;
            return Ok(());
        }
        let tomb = self.root.join(format!("{name}{DELETING}"));
        self.delete(&tomb)?;
        fs::rename(path, &tomb)?;
        sync_dir(&self.root)?;
        if !stop_after_rename {
            fs::remove_dir_all(&tomb)?;
        }
        Ok(())
    }

    /// Writes the pointer. An error before the rename means it didn't move;
    /// once renamed it has, so a failed fsync of the folder after that is
    /// returned inside `Ok`, for [`Switched::leftover`].
    fn write_pointer(&self, p: &Pointer) -> Result<Option<LayoutError>, LayoutError> {
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
        Ok(sync_dir(&self.root).err().map(LayoutError::from))
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
    #[cfg_attr(windows, allow(unused_mut))] // only Unix sets a mode
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
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if fs::symlink_metadata(path)?.file_type().is_symlink_dir() {
            return fs::remove_dir(path);
        }
    }
    fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "lodestar-layout-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_bare_binary_is_staged_and_committed() {
        let d = tmp();
        let mut i = Install::open(&d, &CompanionId::parse("qbittorrent").unwrap()).unwrap();
        let v = Version::parse("5.0.3").unwrap();
        let name = RelPath::parse("qbittorrent-nox").unwrap();
        let big = [7u8; 33];
        assert!(i.stage_binary(&v, &mut &big[..], &name, 32).is_err());
        assert!(!i.version_dir(&v).exists());
        assert!(!i.staging_dir(&v).exists());
        assert_eq!(i.stage_binary(&v, &mut &big[..32], &name, 32).unwrap(), 32);
        assert!(matches!(
            i.stage_binary(&v, &mut &big[..1], &name, 32),
            Err(LayoutError::AlreadyStaged)
        ));
        let nested = RelPath::parse("a/b").unwrap();
        let w = Version::parse("5.0.4").unwrap();
        assert!(i.stage_binary(&w, &mut &big[..1], &nested, 32).is_err());
        i.commit(&v, &name).unwrap();
        assert_eq!(fs::read(i.executable(&v, &name)).unwrap(), big[..32]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(i.executable(&v, &name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_hostile_binary_name_is_refused_before_anything_is_written() {
        // The name is a catalogue `RelPath`: `..`, separators, an empty
        // name and Windows device names never parse.
        for bad in ["../x", "", "a\\b", "CON", "nul.exe", "x:y"] {
            assert!(RelPath::parse(bad).is_err(), "{bad:?}");
        }
        // One that parses but has more than one component is refused by
        // `stage_binary` itself, with nothing left behind.
        let d = tmp();
        let mut i = Install::open(&d, &CompanionId::parse("qbittorrent").unwrap()).unwrap();
        let v = Version::parse("5.0.3").unwrap();
        let nested = RelPath::parse("a/b").unwrap();
        assert!(i.stage_binary(&v, &mut &b"x"[..], &nested, 8).is_err());
        assert_eq!(fs::read_dir(i.root()).unwrap().count(), 0);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn an_interrupted_binary_stage_is_cleaned_up() {
        let d = tmp();
        let mut i = Install::open(&d, &CompanionId::parse("qbittorrent").unwrap()).unwrap();
        let v = Version::parse("5.0.3").unwrap();
        let name = RelPath::parse("qbittorrent").unwrap();
        // A crash mid-copy leaves `<version>.staging/` with a partial file.
        fs::create_dir(i.staging_dir(&v)).unwrap();
        fs::write(i.staging_dir(&v).join("qbittorrent"), b"par").unwrap();
        // The sweep at start-up removes it; so does the next stage.
        assert_eq!(i.sweep().unwrap(), vec!["5.0.3.staging".to_owned()]);
        fs::create_dir(i.staging_dir(&v)).unwrap();
        assert_eq!(i.stage_binary(&v, &mut &b"full"[..], &name, 8).unwrap(), 4);
        assert!(!i.staging_dir(&v).exists());
        assert_eq!(fs::read(i.executable(&v, &name)).unwrap(), b"full");
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn remove_last_keeps_its_marker_until_everything_else_is_gone() {
        let d = tmp();
        let i = Install::open(&d, &CompanionId::parse("prowlarr").unwrap()).unwrap();
        fs::create_dir_all(i.root().join("home/a")).unwrap();
        fs::write(i.root().join("state.json"), b"{}").unwrap();
        fs::write(i.root().join("other"), b"x").unwrap();
        let root = i.root().to_path_buf();
        i.remove_last(Some("state.json")).unwrap();
        assert!(!root.exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn stage_binary_refuses_links_and_cleans_up_a_failed_copy() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("cut"))
            }
        }
        let d = tmp();
        let mut i = Install::open(&d, &CompanionId::parse("qbittorrent").unwrap()).unwrap();
        let v = Version::parse("5.0.3").unwrap();
        let name = RelPath::parse("qbittorrent").unwrap();
        // A reader error mid-copy leaves neither folder.
        assert!(i.stage_binary(&v, &mut Broken, &name, 8).is_err());
        assert!(!i.staging_dir(&v).exists());
        assert!(!i.version_dir(&v).exists());
        #[cfg(unix)]
        {
            let outside = tmp();
            // A link in place of the staging folder or the version folder
            // is refused, and its target is left alone.
            std::os::unix::fs::symlink(&outside, i.staging_dir(&v)).unwrap();
            assert!(i.stage_binary(&v, &mut &b"x"[..], &name, 8).is_err());
            assert!(
                fs::symlink_metadata(i.staging_dir(&v))
                    .unwrap()
                    .is_symlink()
            );
            fs::remove_file(i.staging_dir(&v)).unwrap();
            std::os::unix::fs::symlink(&outside, i.version_dir(&v)).unwrap();
            assert!(matches!(
                i.stage_binary(&v, &mut &b"x"[..], &name, 8),
                Err(LayoutError::AlreadyStaged)
            ));
            assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
            let _ = fs::remove_dir_all(outside);
        }
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn remove_own_spares_what_someone_else_wrote() {
        let d = tmp();
        let i = Install::open(&d, &CompanionId::parse("qbittorrent").unwrap()).unwrap();
        for dir in ["5.0.3", "5.0.4.staging", "x.deleting", "home/a"] {
            fs::create_dir_all(i.root().join(dir)).unwrap();
        }
        fs::write(i.root().join("current"), b"{}").unwrap();
        fs::write(i.root().join("state.json"), b"{}").unwrap();
        fs::write(i.root().join("theirs.iso"), b"mine").unwrap();
        let root = i.root().to_path_buf();
        assert!(!i.remove_own(&["home"], Some("state.json")).unwrap());
        let left: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, ["theirs.iso"]);
        // With nothing foreign left, the folder goes too.
        fs::remove_file(root.join("theirs.iso")).unwrap();
        let i = Install::existing(&d, &CompanionId::parse("qbittorrent").unwrap())
            .unwrap()
            .unwrap();
        assert!(i.remove_own(&[], None).unwrap());
        assert!(!root.exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn the_pointer_round_trips() {
        let d = tmp();
        let i = Install::open(&d, &CompanionId::parse("prowlarr").unwrap()).unwrap();
        let p = Pointer {
            current: Version::parse("5.0.3").unwrap(),
            previous: Some(Version::parse("5.0.2").unwrap()),
        };
        i.write_pointer(&p).unwrap();
        assert_eq!(
            fs::read_to_string(i.root().join(POINTER)).unwrap(),
            r#"{"format":1,"current":"5.0.3","previous":"5.0.2"}"#
        );
        assert_eq!(i.pointer().unwrap(), Some(p));
        assert!(!i.root().join(POINTER_TMP).exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn sweep_takes_only_its_own_names() {
        let d = tmp();
        let mut i = Install::open(&d, &CompanionId::parse("prowlarr").unwrap()).unwrap();
        for n in [
            "1.0",
            "1.0.staging",
            "01.0",
            "1.0.staging.x",
            "config",
            "1.0.old",
        ] {
            fs::create_dir(i.root().join(n)).unwrap();
        }
        fs::write(i.root().join(POINTER_TMP), b"x").unwrap();
        assert_eq!(i.sweep().unwrap(), ["1.0", "1.0.staging", POINTER_TMP]);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn delete_takes_only_direct_children() {
        let d = tmp();
        let i = Install::open(&d, &CompanionId::parse("prowlarr").unwrap()).unwrap();
        fs::create_dir_all(i.root().join("a/b")).unwrap();
        assert!(matches!(
            i.delete(&i.root().join("a/b")),
            Err(LayoutError::OutsideRoot)
        ));
        assert!(matches!(i.delete(&d), Err(LayoutError::OutsideRoot)));
        assert!(matches!(i.delete(i.root()), Err(LayoutError::OutsideRoot)));
        i.delete(&i.root().join("a")).unwrap();
        assert!(!i.root().join("a").exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_folder_is_deleted_by_way_of_a_tombstone() {
        let d = tmp();
        let mut i = Install::open(&d, &CompanionId::parse("prowlarr").unwrap()).unwrap();
        fs::create_dir_all(i.root().join("1.0/app")).unwrap();
        // A crash right after the rename leaves only the tombstone, never
        // a partial `1.0`.
        i.delete_steps(&i.root().join("1.0"), true).unwrap();
        assert!(!i.root().join("1.0").exists());
        assert!(i.root().join("1.0.deleting/app").is_dir());
        assert_eq!(i.sweep().unwrap(), ["1.0.deleting"]);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn remove_drops_the_pointer_first() {
        let d = tmp();
        let mut i = Install::open(&d, &CompanionId::parse("prowlarr").unwrap()).unwrap();
        fs::create_dir_all(i.root().join("1.0/app")).unwrap();
        i.write_pointer(&Pointer {
            current: Version::parse("1.0").unwrap(),
            previous: None,
        })
        .unwrap();
        // A crash right after the first step: no pointer, so nothing
        // names the half-deleted version and a sweep takes it.
        i.remove_pointer().unwrap();
        assert_eq!(i.pointer().unwrap(), None);
        assert_eq!(i.sweep().unwrap(), ["1.0"]);
        i.remove().unwrap();
        assert!(!d.join("companions/prowlarr").exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn nothing_is_swept_when_the_pointer_may_not_be_durable() {
        let d = tmp();
        let mut i = Install::open(&d, &CompanionId::parse("prowlarr").unwrap()).unwrap();
        for v in ["1.0", "2.0"] {
            fs::create_dir_all(i.root().join(v)).unwrap();
        }
        // A rollback from 2.0 to 1.0 whose rename wasn't fsynced: 2.0,
        // which the old pointer named, must survive.
        let new = Pointer {
            current: Version::parse("1.0").unwrap(),
            previous: None,
        };
        assert!(i.write_pointer(&new).unwrap().is_none());
        let unsynced = Some(LayoutError::Io(io::ErrorKind::Other));
        let done = i.after_switch(new.clone(), unsynced);
        assert!(matches!(done.leftover, Some(LayoutError::Io(_))));
        assert!(i.root().join("2.0").is_dir());
        // With the fsync done, the cleanup runs.
        let done = i.after_switch(new, None);
        assert!(done.leftover.is_none());
        assert!(!i.root().join("2.0").exists());
        let _ = fs::remove_dir_all(d);
    }
}
