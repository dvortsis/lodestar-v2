//! The watch folder (UP-1, `docs/decisions/2026-11-12-UP-1.md`): one
//! folder whose `.torrent` files are taken exactly as uploads are. Off
//! while `folder` is empty, which is the default.
//!
//! Saved in the app only: no `config.toml` key or `LODESTAR_*` variable
//! backs it yet (runner, 2026-11-12: `LODESTAR_WATCH_DIR` is follow-up
//! X-watch-env). Both fields apply live: [`crate::watch`] follows the
//! section and checks the folder again each time it changes and at
//! start.
//!
//! [`Section::check`] sees only this section, not the data directory, so
//! a save checks what it can (absolute, there, a directory and not a
//! link) and [`folder_problem`] checks the rest when the setting is
//! applied: a folder that is the data directory, is inside it or holds
//! it is never read, and the watch shows why.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Field, Section};
use crate::config::Config;

/// What happens to a file once it's been taken.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum After {
    /// Moved into `added/` or `failed/` inside the folder.
    #[default]
    Move,
    /// Deleted.
    Delete,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    /// The folder to watch for `.torrent` files, as an absolute path;
    /// empty turns the watch off.
    pub folder: String,
    // Undocumented here: a field's own description would wrap the
    // inlined enum in a `oneOf`.
    #[schema(inline)]
    pub after: After,
}

impl Watch {
    /// The folder, when one is set.
    pub fn folder(&self) -> Option<PathBuf> {
        (!self.folder.trim().is_empty()).then(|| PathBuf::from(self.folder.trim()))
    }
}

impl Section for Watch {
    const SECTION: &'static str = "watch";
    const FIELDS: &'static [Field] = &[
        Field::new("folder").group("watch"),
        Field::new("after").group("watch").widget("radio"),
    ];

    /// No `config.toml` key backs these yet.
    fn from_config(_: &Config) -> Self {
        Watch::default()
    }

    fn apply(&self, _: &str, _: &mut Config) {}

    fn check(&self, field: &str) -> Result<(), String> {
        if field != "folder" {
            return Ok(());
        }
        match self.folder() {
            None => Ok(()),
            Some(folder) => match shape_problem(&folder) {
                None => Ok(()),
                Some(p) => Err(p.message().into()),
            },
        }
    }
}

/// Why a folder can't be watched. A fixed cause, never the path: the
/// status and log lines carry it as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderProblem {
    NotAbsolute,
    /// It isn't there, or can't be looked at.
    Missing,
    NotDirectory,
    /// The folder itself is a symbolic link, junction or other reparse
    /// point.
    Link,
    /// It is the data directory, or inside it.
    InsideDataDir,
    /// The data directory is inside it.
    HoldsDataDir,
}

impl FolderProblem {
    /// A stable code for status and logs.
    pub fn code(self) -> &'static str {
        match self {
            FolderProblem::NotAbsolute => "not_absolute",
            FolderProblem::Missing => "missing",
            FolderProblem::NotDirectory => "not_directory",
            FolderProblem::Link => "link",
            FolderProblem::InsideDataDir => "inside_data_dir",
            FolderProblem::HoldsDataDir => "holds_data_dir",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            FolderProblem::NotAbsolute => "Use a full path, such as /watch or D:\\Torrents.",
            FolderProblem::Missing => "That folder doesn't exist, or Lodestar can't open it.",
            FolderProblem::NotDirectory => "That's a file, not a folder.",
            FolderProblem::Link => "Use the folder itself, not a link to it.",
            FolderProblem::InsideDataDir => {
                "That folder is Lodestar's data folder or inside it. Choose another one."
            }
            FolderProblem::HoldsDataDir => {
                "Lodestar's data folder is inside that folder. Choose another one."
            }
        }
    }
}

/// What a save can check without the data directory: absolute, there, a
/// directory, and not itself a link (its own entry is read without
/// following it).
pub fn shape_problem(folder: &Path) -> Option<FolderProblem> {
    if !folder.is_absolute() {
        return Some(FolderProblem::NotAbsolute);
    }
    let Ok(meta) = std::fs::symlink_metadata(folder) else {
        return Some(FolderProblem::Missing);
    };
    if is_link(&meta) {
        return Some(FolderProblem::Link);
    }
    if !meta.is_dir() {
        return Some(FolderProblem::NotDirectory);
    }
    None
}

/// Everything that keeps `folder` from being watched with `data_dir` as
/// the data directory: [`shape_problem`], then whether either holds the
/// other once both are resolved. A data directory that can't be resolved
/// is compared as written, made absolute.
pub fn folder_problem(folder: &Path, data_dir: &Path) -> Option<FolderProblem> {
    if let Some(p) = shape_problem(folder) {
        return Some(p);
    }
    let Ok(folder) = std::fs::canonicalize(folder) else {
        return Some(FolderProblem::Missing);
    };
    let data = std::fs::canonicalize(data_dir)
        .or_else(|_| std::path::absolute(data_dir))
        .unwrap_or_else(|_| data_dir.to_path_buf());
    if folder.starts_with(&data) {
        return Some(FolderProblem::InsideDataDir);
    }
    if data.starts_with(&folder) {
        return Some(FolderProblem::HoldsDataDir);
    }
    None
}

/// A symbolic link, or on Windows any reparse point (a junction or mount
/// point too), read from metadata that didn't follow it.
pub fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default() {
        let w = Watch::default();
        assert_eq!(w.folder(), None);
        assert_eq!(w.after, After::Move);
        assert!(w.check("folder").is_ok());
        let blank = Watch {
            folder: "   ".into(),
            ..Watch::default()
        };
        assert_eq!(blank.folder(), None);
    }

    #[test]
    fn a_save_checks_the_folders_shape() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.torrent");
        std::fs::write(&file, b"x").unwrap();
        let check = |p: &Path| {
            Watch {
                folder: p.to_string_lossy().into_owned(),
                after: After::Move,
            }
            .check("folder")
        };
        assert!(check(dir.path()).is_ok());
        assert!(check(Path::new("relative/folder")).is_err());
        assert!(check(&dir.path().join("missing")).is_err());
        assert!(check(&file).is_err());
        assert_eq!(
            shape_problem(Path::new("relative")),
            Some(FolderProblem::NotAbsolute)
        );
        assert_eq!(shape_problem(&file), Some(FolderProblem::NotDirectory));
        assert_eq!(
            shape_problem(&dir.path().join("missing")),
            Some(FolderProblem::Missing)
        );
    }

    #[test]
    fn the_data_directory_and_the_folder_never_hold_each_other() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let watch = root.path().join("watch");
        let inside = data.join("incoming");
        for d in [&data, &watch, &inside] {
            std::fs::create_dir_all(d).unwrap();
        }
        assert_eq!(folder_problem(&watch, &data), None);
        assert_eq!(
            folder_problem(&data, &data),
            Some(FolderProblem::InsideDataDir)
        );
        assert_eq!(
            folder_problem(&inside, &data),
            Some(FolderProblem::InsideDataDir)
        );
        assert_eq!(
            folder_problem(root.path(), &data),
            Some(FolderProblem::HoldsDataDir)
        );
        // Through a `..` that resolves into the data directory.
        let sneaky = watch.join("..").join("data");
        assert_eq!(
            folder_problem(&sneaky, &data),
            Some(FolderProblem::InsideDataDir)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_folder_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(shape_problem(&link), Some(FolderProblem::Link));
        // A link to the data directory is refused before it's resolved.
        assert_eq!(folder_problem(&link, &real), Some(FolderProblem::Link));
    }

    #[test]
    fn codes_and_messages_are_fixed_and_name_no_path() {
        for p in [
            FolderProblem::NotAbsolute,
            FolderProblem::Missing,
            FolderProblem::NotDirectory,
            FolderProblem::Link,
            FolderProblem::InsideDataDir,
            FolderProblem::HoldsDataDir,
        ] {
            assert!(
                p.code()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_')
            );
            assert!(!p.message().is_empty());
        }
    }
}
