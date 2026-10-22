//! `lodestar rollback [SNAPSHOT] [--yes]`: lists the snapshots taken
//! before each schema migration (`lodestar.pre-v<N>.db`, architecture
//! §12) and restores one, while the server is stopped.
//!
//! The order keeps every step undoable:
//! 1. refuse while B-update's journal (`bin/rollback.json`) exists, since
//!    the supervisor's own rollback is then in progress (§12.2);
//! 2. lock the store, refusing if any process has it open;
//! 3. check the snapshot, then confirm (unless `--yes`);
//! 4. copy the current store to `lodestar.before-rollback-<time>.db`;
//! 5. move the search index aside (`index.before-rollback-<time>`): its
//!    documents and watermarks follow the current store, and ids reused
//!    after the restore would show other torrents' data;
//! 6. copy the snapshot beside the store and rename it into place.
//!
//! The snapshot itself is kept, so the same restore can be done again.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use super::lock::StoreLock;
use super::util::{
    Failure, Outcome, failed, file_len, file_stamp, format_bytes, format_count, format_time,
    modified_secs, open_read_only, quick_check, refuse_during_update_rollback, remove_if_present,
    schema_version, sync_dir, sync_file, torrent_count, unix_now, vacuum_into,
};
use crate::config::Config;

/// One `<stem>.pre-v<N>[.<k>].db` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: PathBuf,
    pub name: String,
    /// Taken before migrating to this version, so it holds `version - 1`.
    pub version: i64,
    /// The `k` of a repeat snapshot of the same migration (0 for the first).
    pub repeat: u32,
    pub bytes: u64,
    pub modified: Option<i64>,
}

/// The snapshots of the store at `db`, oldest migration first.
pub fn snapshots(db: &Path) -> Outcome<Vec<Snapshot>> {
    let dir = db.parent().unwrap_or(Path::new("."));
    let stem = db
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Failure::Failed(format!("{} has no file name", db.display())))?;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Failure::Failed(format!("reading {}: {e}", dir.display()))),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(failed(format!("reading {}", dir.display())))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some((version, repeat)) = parse_name(stem, &name) else {
            continue;
        };
        // Symlinks and folders aren't snapshots this build wrote.
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        out.push(Snapshot {
            bytes: file_len(&path),
            modified: modified_secs(&path),
            path,
            name,
            version,
            repeat,
        });
    }
    out.sort_by_key(|s| (s.version, s.repeat));
    Ok(out)
}

/// `(N, k)` from `<stem>.pre-v<N>.db` or `<stem>.pre-v<N>.<k>.db`.
fn parse_name(stem: &str, name: &str) -> Option<(i64, u32)> {
    let rest = name
        .strip_prefix(stem)?
        .strip_prefix(".pre-v")?
        .strip_suffix(".db")?;
    let (n, k) = match rest.split_once('.') {
        Some((n, k)) => (n, Some(k)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(n) || !k.is_none_or(digits) {
        return None;
    }
    Some((n.parse().ok()?, k.map_or(Some(0), |k| k.parse().ok())?))
}

/// Which snapshot `wanted` names: its file name, or `v<N>` when exactly
/// one snapshot was taken before migration N. Never a path, so a restore
/// can only come from the store's own folder.
fn pick<'a>(all: &'a [Snapshot], wanted: &str) -> Outcome<&'a Snapshot> {
    if let Some(s) = all.iter().find(|s| s.name == wanted) {
        return Ok(s);
    }
    let by_version: Vec<_> = wanted
        .strip_prefix('v')
        .and_then(|n| n.parse::<i64>().ok())
        .map(|n| all.iter().filter(|s| s.version == n).collect())
        .unwrap_or_default();
    match by_version.as_slice() {
        [one] => Ok(one),
        [] => Err(Failure::Failed(format!(
            "no snapshot {wanted:?}; run `lodestar rollback` to list them"
        ))),
        _ => Err(Failure::Failed(format!(
            "more than one snapshot for {wanted}; name the file instead"
        ))),
    }
}

/// Rollback's options.
#[derive(Debug, Clone, Copy)]
pub struct Options<'a> {
    pub snapshot: Option<&'a str>,
    pub yes: bool,
}

pub fn run(config: &Config, options: Options<'_>) -> Outcome {
    refuse_during_update_rollback(&config.data_dir)?;
    let db = config.db_path();
    let all = snapshots(&db)?;
    let Some(wanted) = options.snapshot else {
        list(&all);
        return Ok(());
    };
    let snapshot = pick(&all, wanted)?.clone();
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let confirm = |summary: &str| -> Outcome<bool> {
        println!("{summary}");
        if options.yes {
            return Ok(true);
        }
        if !interactive {
            return Err(Failure::Refused(
                "no terminal to confirm on; add --yes to restore without asking".into(),
            ));
        }
        Ok(confirm_from(&mut stdin.lock(), &mut std::io::stdout()))
    };
    let restored = restore(config, &snapshot, unix_now(), confirm)?;
    match restored {
        None => println!("Nothing was changed."),
        Some(r) => {
            println!(
                "Restored {}. The store before the rollback is kept as {}.",
                snapshot.name,
                r.kept.display()
            );
            if let Some(index) = r.index_aside {
                println!(
                    "The search index was moved to {}; it's rebuilt from the store at the next \
                     start. Delete that folder once search works again.",
                    index.display()
                );
            }
            println!(
                "Now start the Lodestar version that used schema v{} (the release before the \
                 upgrade). This version would upgrade the store again at its next start.",
                r.schema
            );
        }
    }
    Ok(())
}

fn list(all: &[Snapshot]) {
    if all.is_empty() {
        println!("No migration snapshots. Lodestar takes one before each upgrade of the store.");
        return;
    }
    println!("Snapshots taken before each upgrade of the store:");
    for s in all {
        let when = s
            .modified
            .map_or_else(|| "unknown time".into(), format_time);
        println!(
            "  {:<28} before v{}, {}, {}",
            s.name,
            s.version,
            format_bytes(s.bytes),
            when
        );
    }
    println!(
        "Stop Lodestar, then run `lodestar rollback <name>` to restore one. The current store \
         is kept as a copy."
    );
}

/// `true` only for an explicit "yes".
fn confirm_from(input: &mut impl BufRead, output: &mut impl Write) -> bool {
    let _ = write!(output, "Type yes to restore: ");
    let _ = output.flush();
    let mut line = String::new();
    input.read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("yes")
}

/// What [`restore`] did.
#[derive(Debug)]
pub struct Restored {
    /// The copy of the store as it was.
    pub kept: PathBuf,
    /// Where the search index went, if there was one.
    pub index_aside: Option<PathBuf>,
    /// The restored store's schema version.
    pub schema: i64,
}

/// Restores `snapshot` over the store, once `confirm` (given a summary)
/// agrees; `None` when it doesn't.
pub fn restore(
    config: &Config,
    snapshot: &Snapshot,
    now: i64,
    confirm: impl FnOnce(&str) -> Outcome<bool>,
) -> Outcome<Option<Restored>> {
    let db = config.db_path();
    let lock = StoreLock::acquire(&db)?;
    let (snap_schema, snap_torrents) = {
        let conn = open_read_only(&snapshot.path)?;
        quick_check(&conn).map_err(|e| {
            Failure::Failed(format!("{} fails its check: {e}", snapshot.path.display()))
        })?;
        (
            schema_version(&conn).map_err(failed("reading the snapshot's schema"))?,
            torrent_count(&conn).map_err(failed("counting the snapshot's torrents"))?,
        )
    };
    let cur_schema = schema_version(lock.conn()).map_err(failed("reading the store's schema"))?;
    let cur_torrents = torrent_count(lock.conn()).map_err(failed("counting the torrents"))?;
    let count = |n: Option<u64>| n.map_or_else(|| "no".into(), format_count);
    let dir = db.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = db
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("lodestar");
    let stamp = file_stamp(now);
    let kept = dir.join(format!("{stem}.before-rollback-{stamp}.db"));
    let index_root = config
        .index_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config.data_dir.join("index"));
    let index_aside = index_root.with_file_name(format!(
        "{}.before-rollback-{stamp}",
        index_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("index")
    ));
    let summary = format!(
        "Restore {} (schema v{snap_schema}, {} torrents, {})\n\
         over the current store (schema v{cur_schema}, {} torrents).\n\
         Torrents stored since the snapshot won't be in the restored store. The current store \
         is kept as {}, and the search index is rebuilt.",
        snapshot.name,
        count(snap_torrents),
        snapshot
            .modified
            .map_or_else(|| "unknown time".into(), format_time),
        count(cur_torrents),
        kept.display(),
    );
    if !confirm(&summary)? {
        lock.release()?;
        return Ok(None);
    }

    // 4. The current store, kept.
    vacuum_into(lock.conn(), &kept)?;
    // 5. The index, moved aside (a rename: no copy, no space).
    let index_aside = if index_root.exists() {
        std::fs::rename(&index_root, &index_aside).map_err(failed(format!(
            "moving the search index {} aside (nothing was restored)",
            index_root.display()
        )))?;
        sync_dir(&dir);
        Some(index_aside)
    } else {
        None
    };
    // From here a failure leaves the store as it was, but the index has
    // moved: say where, and that the next start rebuilds it.
    let index_note = |e: Failure| match (&index_aside, e) {
        (Some(aside), Failure::Failed(m)) => Failure::Failed(format!(
            "{m}. The store wasn't restored. The search index was moved to {} and is rebuilt \
             from the store at the next start; delete that folder once search works again.",
            aside.display()
        )),
        (_, e) => e,
    };
    // 6. The snapshot, copied beside the store and renamed into place.
    let staged = dir.join(format!("{stem}.rollback-{stamp}.tmp"));
    remove_if_present(&staged)
        .map_err(failed(format!("removing {}", staged.display())))
        .map_err(index_note)?;
    let copied = std::fs::copy(&snapshot.path, &staged)
        .map_err(failed(format!("copying {}", snapshot.path.display())))
        .and_then(|_| sync_file(&staged).map_err(failed(format!("syncing {}", staged.display()))));
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&staged);
        lock.release()?;
        return Err(index_note(e));
    }
    if let Err(e) = lock.replace_with(&staged) {
        let _ = std::fs::remove_file(&staged);
        return Err(index_note(e));
    }
    Ok(Some(Restored {
        kept,
        index_aside,
        schema: snap_schema,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_snapshot_names() {
        assert_eq!(parse_name("lodestar", "lodestar.pre-v4.db"), Some((4, 0)));
        assert_eq!(parse_name("lodestar", "lodestar.pre-v4.2.db"), Some((4, 2)));
        assert_eq!(parse_name("lodestar", "lodestar.pre-v.db"), None);
        assert_eq!(parse_name("lodestar", "lodestar.pre-v4..db"), None);
        assert_eq!(parse_name("lodestar", "lodestar.pre-v4.x.db"), None);
        assert_eq!(parse_name("lodestar", "lodestar.pre-v4.db-wal"), None);
        assert_eq!(parse_name("lodestar", "other.pre-v4.db"), None);
        assert_eq!(parse_name("lodestar", "lodestar.pre-v-1.db"), None);
        assert_eq!(parse_name("lodestar", "lodestar.pre-v99999999999.db"), None);
        assert_eq!(
            parse_name("lodestar", "lodestar.before-rollback-20261022-000000.db"),
            None
        );
    }

    fn snap(name: &str, version: i64, repeat: u32) -> Snapshot {
        Snapshot {
            path: PathBuf::from(name),
            name: name.into(),
            version,
            repeat,
            bytes: 0,
            modified: None,
        }
    }

    #[test]
    fn picks_by_name_or_unique_version_only() {
        let all = [
            snap("lodestar.pre-v3.db", 3, 0),
            snap("lodestar.pre-v4.db", 4, 0),
            snap("lodestar.pre-v4.1.db", 4, 1),
        ];
        assert_eq!(pick(&all, "lodestar.pre-v4.1.db").unwrap().repeat, 1);
        assert_eq!(pick(&all, "v3").unwrap().name, "lodestar.pre-v3.db");
        assert!(pick(&all, "v4").is_err());
        assert!(pick(&all, "v9").is_err());
        assert!(pick(&all, "../lodestar.pre-v3.db").is_err());
        assert!(pick(&all, "/tmp/lodestar.pre-v3.db").is_err());
    }

    #[test]
    fn only_yes_confirms() {
        let ask = |input: &str| confirm_from(&mut input.as_bytes(), &mut Vec::new());
        assert!(ask("yes\n"));
        assert!(ask(" YES \n"));
        assert!(!ask("y\n"));
        assert!(!ask("no\n"));
        assert!(!ask(""));
    }
}
