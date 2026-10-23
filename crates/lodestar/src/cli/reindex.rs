//! `lodestar reindex`: rebuild the search index from the store
//! (architecture §7, §8).
//!
//! With the server stopped, it marks this build's index for rebuild by
//! renaming its directory (and any unfinished `.building` one) to
//! `….trash`, under the store lock so no server is using it. At the next
//! start `Serving::open_or_rebuild` finds no index for this version and
//! builds one from the store; the trash is deleted by its usual sweep.
//!
//! A running server can't be asked to rebuild yet: the API has no
//! endpoint for it. Until it has, the command says so and changes
//! nothing.

use std::path::{Path, PathBuf};

use super::lock::StoreLock;
use super::util::{Failure, Outcome, failed, refuse_during_update_rollback, sync_dir};
use crate::config::Config;

pub fn run(config: &Config) -> Outcome {
    let moved = reindex(config, || {
        crate::healthcheck(crate::settings::network::effective_bind(config)).is_ok()
    })?;
    if moved.is_empty() {
        println!("There's no search index yet. Lodestar builds one from the store when it starts.");
    } else {
        println!(
            "The search index is marked for rebuild. Lodestar rebuilds it from the store when \
             it next starts; results fill in while it runs."
        );
    }
    Ok(())
}

/// Marks the index for rebuild; returns the directories moved to trash.
/// `server_up` tells a running server apart from another command.
pub fn reindex(config: &Config, server_up: impl FnOnce() -> bool) -> Outcome<Vec<PathBuf>> {
    refuse_during_update_rollback(&config.data_dir)?;
    let db = config.db_path();
    if !db.is_file() {
        return Err(Failure::Failed(format!(
            "no store at {}; nothing to index",
            db.display()
        )));
    }
    let lock = match StoreLock::acquire(&db) {
        Ok(lock) => lock,
        Err(Failure::Refused(_)) if server_up() => {
            return Err(Failure::Refused(
                "Lodestar is running, and this version can't ask a running server to rebuild \
                 its index. Stop Lodestar, run `lodestar reindex`, then start it again."
                    .into(),
            ));
        }
        Err(e) => return Err(e),
    };
    let current = config.index_path();
    let moved = mark_for_rebuild(&current);
    lock.release()?;
    moved
}

/// Renames `current` (`<root>/v{schema}-t{tokenizer}`) and its
/// `.building` sibling to fresh `….trash` names.
fn mark_for_rebuild(current: &Path) -> Outcome<Vec<PathBuf>> {
    let Some(root) = current.parent() else {
        return Ok(Vec::new());
    };
    let name = current
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let mut moved = Vec::new();
    for dir in [current.to_path_buf(), root.join(format!("{name}.building"))] {
        if !dir.is_dir() {
            continue;
        }
        let trash = trash_name(&dir);
        std::fs::rename(&dir, &trash).map_err(failed(format!(
            "moving {} to {}",
            dir.display(),
            trash.display()
        )))?;
        moved.push(trash);
    }
    if !moved.is_empty() {
        sync_dir(root);
    }
    Ok(moved)
}

/// `<dir>.trash`, or `<dir>.<k>.trash` if that's taken.
fn trash_name(dir: &Path) -> PathBuf {
    let base = dir.as_os_str().to_os_string();
    (0u32..)
        .map(|k| {
            let mut name = base.clone();
            match k {
                0 => name.push(".trash"),
                k => name.push(format!(".{k}.trash")),
            }
            PathBuf::from(name)
        })
        .find(|p| !p.exists())
        .expect("unbounded search")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_this_versions_index_and_build_to_trash() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("index");
        let current = root.join("v9-t2");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(root.join("v9-t2.building")).unwrap();
        std::fs::create_dir_all(root.join("v9-t2.trash")).unwrap();
        std::fs::create_dir_all(root.join("v8-t2")).unwrap();
        let moved = mark_for_rebuild(&current).unwrap();
        assert_eq!(
            moved,
            [
                root.join("v9-t2.1.trash"),
                root.join("v9-t2.building.trash")
            ]
        );
        assert!(!current.exists());
        assert!(!root.join("v9-t2.building").exists());
        // Other versions are the rebuild's business.
        assert!(root.join("v8-t2").exists());
        assert!(mark_for_rebuild(&current).unwrap().is_empty());
    }
}
