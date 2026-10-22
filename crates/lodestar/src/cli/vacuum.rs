//! `lodestar vacuum`: compacts the store while the server is stopped and
//! reports its size before and after.
//!
//! It runs `VACUUM INTO` a new file in the store's own folder, checks it,
//! then renames it over the store ([`StoreLock::replace_with`]), rather
//! than a plain `VACUUM`. That needs the same free space (one more copy),
//! but the copy is built on the store's own disk instead of the system
//! temporary folder (in a container, the image's layer: on Unraid,
//! `docker.img`), and a crash leaves either store whole. The page size
//! and incremental auto-vacuum carry over.

use super::lock::StoreLock;
use super::util::{
    Failure, Outcome, failed, file_len, format_bytes, open_read_only, quick_check,
    refuse_during_update_rollback, remove_if_present, store_bytes, vacuum_into,
};
use crate::config::Config;

/// Sizes before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub before: u64,
    pub after: u64,
}

pub fn run(config: &Config) -> Outcome {
    let r = vacuum(config)?;
    println!(
        "Vacuumed {}: {} before, {} after ({} freed).",
        config.db_path().display(),
        format_bytes(r.before),
        format_bytes(r.after),
        format_bytes(r.before.saturating_sub(r.after))
    );
    Ok(())
}

pub fn vacuum(config: &Config) -> Outcome<Report> {
    refuse_during_update_rollback(&config.data_dir)?;
    let db = config.db_path();
    // Measured before the lock folds the WAL in.
    let before = store_bytes(&db);
    let lock = StoreLock::acquire(&db)?;
    let staged = {
        let mut name = db.as_os_str().to_os_string();
        name.push(".vacuum.tmp");
        std::path::PathBuf::from(name)
    };
    // Ours, from an attempt that was cut short: the store is untouched.
    remove_if_present(&staged).map_err(failed(format!("removing {}", staged.display())))?;
    let built = vacuum_into(lock.conn(), &staged).and_then(|()| {
        let conn = open_read_only(&staged)?;
        quick_check(&conn).map_err(|e| Failure::Failed(format!("the compacted copy: {e}")))
    });
    if let Err(e) = built {
        let _ = std::fs::remove_file(&staged);
        lock.release()?;
        return Err(e);
    }
    if let Err(e) = lock.replace_with(&staged) {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    Ok(Report {
        before,
        after: file_len(&db),
    })
}
