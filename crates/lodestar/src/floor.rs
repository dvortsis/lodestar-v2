//! The locked floor from config files and floor-marked lists only adds
//! (owner's decision §2.7). At start, `blocklist.files` add their entries
//! through the store; entries a file no longer lists, and entries a
//! floor-marked list's publisher removed, stay blocked. They're reported
//! once per start and wait in the pending floor removals, which the
//! filters API shows and settles with the effect warning and the password.

use std::sync::atomic::{AtomicU64, Ordering};

use lodestar_store::{FloorSource, Store};

/// Floor entries a config file or a floor-marked list dropped and the
/// floor kept, counted at each start and each list refresh.
pub static KEPT: AtomicU64 = AtomicU64::new(0);

/// Adds the configured files' entries to the floor, and settles what each
/// file (configured or not any more) dropped into its pending removals.
pub fn sync_files(
    store: &mut Store,
    files: &[crate::config::BlocklistFile],
    now: i64,
) -> Result<(), String> {
    for (path, hashes) in files {
        store
            .sync_floor_file(path, hashes, now)
            .map_err(|e| format!("blocklist {path}: {e}"))?;
    }
    for path in store.floor_files().map_err(|e| e.to_string())? {
        if !files.iter().any(|(p, _)| *p == path) {
            store
                .sync_floor_file(&path, &[], now)
                .map_err(|e| format!("blocklist {path}: {e}"))?;
        }
    }
    Ok(())
}

/// The start-up line for one source's pending removals.
pub fn message(store: &Store, source: &FloorSource, n: usize) -> String {
    let entries = if n == 1 { "entry was" } else { "entries were" };
    match source {
        FloorSource::File(path) => {
            format!("{n} floor {entries} removed from {path}; remove them in the app to unblock")
        }
        FloorSource::List(id) => {
            let name = store
                .subscription(*id)
                .ok()
                .flatten()
                .map_or_else(|| format!("list {id}"), |s| s.name);
            format!(
                "{n} floor {entries} removed upstream from {name}; remove them in the app to unblock"
            )
        }
    }
}

/// Reports every source's pending floor removals once, and counts them.
/// Returns the lines logged.
pub fn report(store: &Store) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for p in store.pending_floor().map_err(|e| e.to_string())? {
        let line = message(store, &p.source, p.hashes.len());
        tracing::warn!("{line}");
        KEPT.fetch_add(p.hashes.len() as u64, Ordering::Relaxed);
        lines.push(line);
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_store::StoreConfig;

    #[test]
    fn files_only_add_and_drops_are_reported() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let (a, b) = ([1; 20], [2; 20]);
        let files = |hs: &[[u8; 20]]| vec![("blocked.txt".to_owned(), hs.to_vec())];
        sync_files(&mut store, &files(&[a, b]), 1).unwrap();
        assert!(report(&store).unwrap().is_empty());
        // `b` deleted from the file: still in the floor.
        sync_files(&mut store, &files(&[a]), 2).unwrap();
        let before = KEPT.load(Ordering::Relaxed);
        assert_eq!(
            report(&store).unwrap(),
            ["1 floor entry was removed from blocked.txt; remove them in the app to unblock"]
        );
        assert!(KEPT.load(Ordering::Relaxed) > before);
        let cs = store.filter_state().unwrap().cs_hashes;
        assert!(cs.iter().any(|(h, _)| *h == b));
        // The file dropped from the config: everything it added waits.
        sync_files(&mut store, &[], 3).unwrap();
        let lines = report(&store).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("2 floor entries were removed from blocked.txt"));
        assert_eq!(store.filter_state().unwrap().cs_hashes.len(), 2);
    }
}
