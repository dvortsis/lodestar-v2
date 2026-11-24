//! The watch folder (UP-1, `docs/decisions/2026-11-12-UP-1.md`): one
//! folder whose `.torrent` files are taken exactly as uploads are. Off
//! while `folder` is empty, which is the default.
//!
//! `folder` is also `[watch] folder` in `config.toml` and
//! `LODESTAR_WATCH_DIR` (X-watch-env, for Docker), layered like every
//! other field: a saved value beats the file's, and the variable beats
//! both and makes the field read-only here. A blank variable counts as
//! unset. `after` is saved in the app only. Both fields apply live:
//! [`crate::watch`] follows the section and checks the folder again each
//! time it changes and at start.
//!
//! What's checked, and when (rv-765 B1):
//! - **On every load,** saved values included: only that the folder is
//!   empty or an absolute path ([`Section::check`]). The file's and the
//!   variable's value get the same check in `Config::load`, which refuses
//!   to start on a relative one. Nothing about the
//!   disk, so a share that isn't mounted yet at start keeps its setting.
//! - **On a save** ([`Section::confirm`], which only a patch or an import
//!   runs): also that it's there, a directory, and not a link.
//! - **When the setting is applied and on every poll** ([`folder_problem`],
//!   in [`crate::watch`]): all of that again, plus the data-directory
//!   rule. A folder that is the data directory, is inside it or holds it
//!   is never read; a missing one pauses the watch, which resumes once
//!   it's there.

use std::path::{Path, PathBuf};

use lodestar_api::settings::SettingsError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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
    /// empty turns the watch off. Also set in `config.toml` or
    /// `LODESTAR_WATCH_DIR`.
    pub folder: String,
    // Undocumented here: a field's own description would wrap the
    // inlined enum in a `oneOf`.
    #[schema(inline)]
    pub after: After,
}

impl Watch {
    /// The folder, when one is set, rebuilt from its components: a
    /// trailing separator or `.` is dropped, since on Unix `lstat` and
    /// `O_NOFOLLOW` resolve a last component written as `link/`, so the
    /// checks that the folder isn't a link would pass for one.
    pub fn folder(&self) -> Option<PathBuf> {
        let raw = self.folder.trim();
        (!raw.is_empty()).then(|| Path::new(raw).components().collect())
    }
}

impl Section for Watch {
    const SECTION: &'static str = "watch";
    const FIELDS: &'static [Field] = &[
        Field::new("folder")
            .file("watch.folder")
            .env("LODESTAR_WATCH_DIR")
            .group("watch"),
        Field::new("after").group("watch").widget("radio"),
    ];

    /// `folder` from `[watch] folder` or `LODESTAR_WATCH_DIR`; no key
    /// backs `after`.
    fn from_config(config: &Config) -> Self {
        Watch {
            folder: config.watch.folder.clone(),
            ..Watch::default()
        }
    }

    fn apply(&self, field: &str, config: &mut Config) {
        if field == "folder" {
            config.watch.folder = self.folder.clone();
        }
    }

    /// Only what doesn't depend on the disk now: it runs on saved values
    /// at every start too, where a folder that isn't there yet (a share
    /// mounted later) must keep its setting.
    fn check(&self, field: &str) -> Result<(), String> {
        if field != "folder" {
            return Ok(());
        }
        match self.folder() {
            Some(folder) if !folder.is_absolute() => {
                Err(FolderProblem::NotAbsolute.message().into())
            }
            _ => Ok(()),
        }
    }

    /// On a save only: the folder sent is there, a directory and not a
    /// link.
    fn confirm(patch: &Map<String, Value>) -> Result<(), SettingsError> {
        let Some(Value::String(raw)) = patch.get("folder") else {
            return Ok(());
        };
        let sent = Watch {
            folder: raw.clone(),
            ..Watch::default()
        };
        match sent.folder().and_then(|f| shape_problem(&f)) {
            None => Ok(()),
            Some(p) => Err(SettingsError::Invalid {
                field: "folder".into(),
                message: p.message().into(),
            }),
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

    fn confirm_folder(p: &str) -> Result<(), SettingsError> {
        let mut m = Map::new();
        m.insert("folder".into(), Value::String(p.into()));
        Watch::confirm(&m)
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
        // Every load: absolute or empty only.
        assert!(check(dir.path()).is_ok());
        assert!(check(Path::new("relative/folder")).is_err());
        assert!(check(&dir.path().join("missing")).is_ok());
        assert!(check(&file).is_ok());
        // A save: the disk too.
        let confirm = |p: &Path| confirm_folder(&p.to_string_lossy());
        assert!(confirm(dir.path()).is_ok());
        assert!(confirm(&dir.path().join("missing")).is_err());
        assert!(confirm(&file).is_err());
        assert!(confirm_folder("").is_ok());
        assert!(Watch::confirm(&Map::new()).is_ok());
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

    /// A trailing separator or `.` is dropped before any check, so a link
    /// written as `link/` is still seen as a link.
    #[test]
    fn the_folder_is_normalised_before_it_is_checked() {
        let sep = std::path::MAIN_SEPARATOR;
        let w = |s: String| Watch {
            folder: s,
            after: After::Move,
        };
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().to_path_buf();
        for written in [
            format!("{}{sep}", plain.display()),
            format!("{}{sep}.", plain.display()),
            format!("{}{sep}.{sep}", plain.display()),
        ] {
            assert_eq!(w(written).folder(), Some(plain.clone()));
        }
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&plain, &link).unwrap();
            let folder = w(format!("{}/", link.display())).folder().unwrap();
            assert_eq!(shape_problem(&folder), Some(FolderProblem::Link));
            assert!(confirm_folder(&format!("{}/", link.display())).is_err());
        }
    }

    /// A saved folder that's missing at the next start (a share not
    /// mounted yet) keeps its setting: the watch reports it missing and
    /// resumes once it's there (rv-765 B1).
    #[test]
    fn a_saved_folder_missing_at_start_keeps_its_setting() {
        use crate::settings::tests::{config, patch, registry};
        use lodestar_api::settings::SettingsApi;
        let dir = tempfile::tempdir().unwrap();
        let mounts = tempfile::tempdir().unwrap();
        let share = mounts.path().join("share");
        std::fs::create_dir(&share).unwrap();
        let c = config(dir.path(), None, &[]);
        let folder = share.to_string_lossy().into_owned();
        registry(dir.path(), &c)
            .patch("watch", patch(serde_json::json!({ "folder": folder })))
            .unwrap();
        std::fs::remove_dir(&share).unwrap();
        // The next start, with the share not there yet.
        let w = registry(dir.path(), &c).get::<Watch>();
        assert_eq!(w.folder, folder);
        let watcher = crate::watch::Watcher::new(&w, dir.path()).expect("still on");
        assert_eq!(
            watcher.check(),
            Some(crate::watch::Problem::Folder(FolderProblem::Missing))
        );
        std::fs::create_dir(&share).unwrap();
        assert_eq!(watcher.check(), None);
        // A save of a missing folder is still refused.
        std::fs::remove_dir(&share).unwrap();
        assert!(
            registry(dir.path(), &c)
                .patch("watch", patch(serde_json::json!({ "folder": folder })))
                .is_err()
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

    /// Where `LODESTAR_DATA_DIR` is `data` and `LODESTAR_WATCH_DIR` is
    /// `var` (unset when `None`), reading `file` when given.
    fn load(file: Option<&Path>, data: &Path, var: Option<&Path>) -> Config {
        let data = data.to_string_lossy().into_owned();
        let var = var.map(|v| v.to_string_lossy().into_owned());
        Config::load(file, |k| match k {
            "LODESTAR_DATA_DIR" => Some(data.clone()),
            "LODESTAR_WATCH_DIR" => var.clone(),
            _ => None,
        })
        .unwrap()
    }

    /// X-watch-env: `[watch] folder` and `LODESTAR_WATCH_DIR` are layered
    /// like every field: a saved value beats the file's, and the variable
    /// beats both, locks the field and never erases the saved value.
    #[test]
    fn the_folder_from_the_file_and_the_environment_is_layered() {
        use crate::settings::tests::{patch, registry};
        use lodestar_api::settings::{SettingSource, SettingsApi};
        let root = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let [from_file, saved, from_env] = ["a", "b", "c"].map(|n| root.path().join(n));
        for d in [&data, &from_file, &saved, &from_env] {
            std::fs::create_dir(d).unwrap();
        }
        let toml = root.path().join("config.toml");
        let text = format!("[watch]\nfolder = '{}'\n", from_file.display());
        std::fs::write(&toml, text).unwrap();
        let text = |p: &Path| Value::String(p.to_string_lossy().into_owned());
        let field = |r: &crate::settings::Registry| {
            let f = r.section("watch").unwrap().fields["folder"].clone();
            (f.value, f.source, f.read_only)
        };
        // The file's.
        let c = load(Some(&toml), &data, None);
        let r = registry(db.path(), &c);
        assert_eq!(field(&r), (text(&from_file), SettingSource::File, false));
        assert_eq!(r.get::<Watch>().folder(), Some(from_file.clone()));
        // A saved one beats it.
        r.patch(
            "watch",
            patch(serde_json::json!({ "folder": text(&saved) })),
        )
        .unwrap();
        assert_eq!(field(&r), (text(&saved), SettingSource::Db, false));
        // The variable beats both, and the app can't change it.
        let c = load(Some(&toml), &data, Some(&from_env));
        let r = registry(db.path(), &c);
        assert_eq!(field(&r), (text(&from_env), SettingSource::Env, true));
        assert_eq!(r.get::<Watch>().folder(), Some(from_env.clone()));
        assert!(matches!(
            r.patch("watch", patch(serde_json::json!({ "folder": text(&saved) }))),
            Err(SettingsError::SetByEnv { ref var, .. }) if var == "LODESTAR_WATCH_DIR"
        ));
        // `after` stays the app's to change.
        r.patch("watch", patch(serde_json::json!({ "after": "delete" })))
            .unwrap();
        assert_eq!(r.get::<Watch>().after, After::Delete);
        // Without the variable the saved value is back.
        let c = load(Some(&toml), &data, None);
        let r = registry(db.path(), &c);
        assert_eq!(field(&r), (text(&saved), SettingSource::Db, false));
    }

    /// A folder from the environment gets every rule a saved one does:
    /// the watch built from it stays off with the cause (`watch.disabled`)
    /// until it's fixed, and it's never read.
    #[test]
    fn a_folder_from_the_environment_gets_every_check() {
        use crate::settings::tests::registry;
        let root = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let watch = root.path().join("watch");
        let inside = data.join("incoming");
        for d in [&data, &watch, &inside] {
            std::fs::create_dir_all(d).unwrap();
        }
        let file = root.path().join("a.torrent");
        std::fs::write(&file, b"x").unwrap();
        let check = |var: &Path| {
            let c = load(None, &data, Some(var));
            let w = registry(db.path(), &c).get::<Watch>();
            assert_eq!(w.folder, var.to_string_lossy());
            crate::watch::Watcher::new(&w, &c.data_dir)
                .expect("set")
                .check()
        };
        let folder = |p| Some(crate::watch::Problem::Folder(p));
        assert_eq!(check(&watch), None);
        assert_eq!(
            check(&root.path().join("missing")),
            folder(FolderProblem::Missing)
        );
        assert_eq!(check(&file), folder(FolderProblem::NotDirectory));
        assert_eq!(check(&data), folder(FolderProblem::InsideDataDir));
        assert_eq!(check(&inside), folder(FolderProblem::InsideDataDir));
        assert_eq!(check(root.path()), folder(FolderProblem::HoldsDataDir));
        // Through a `..` that resolves into the data directory.
        let sneaky = watch.join("..").join("data").join("incoming");
        assert_eq!(check(&sneaky), folder(FolderProblem::InsideDataDir));
        #[cfg(unix)]
        {
            // A link, to the data directory or anywhere else, is refused
            // before it's resolved.
            let to_data = root.path().join("to-data");
            std::os::unix::fs::symlink(&data, &to_data).unwrap();
            assert_eq!(check(&to_data), folder(FolderProblem::Link));
            let to_watch = root.path().join("to-watch");
            std::os::unix::fs::symlink(&watch, &to_watch).unwrap();
            assert_eq!(check(&to_watch), folder(FolderProblem::Link));
        }
        // A missing folder that appears later is watched, as a saved one
        // is (rv-765 B1).
        let later = root.path().join("later");
        let c = load(None, &data, Some(&later));
        let w = registry(db.path(), &c).get::<Watch>();
        let watcher = crate::watch::Watcher::new(&w, &c.data_dir).unwrap();
        assert_eq!(watcher.check(), folder(FolderProblem::Missing));
        std::fs::create_dir(&later).unwrap();
        assert_eq!(watcher.check(), None);
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
