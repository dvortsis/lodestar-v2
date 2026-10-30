//! `lodestar backup [--to PATH]`: a consistent copy of everything that
//! can't be rebuilt, safe while the server runs (architecture §11).
//!
//! The store is copied with `VACUUM INTO` on a read-only connection: one
//! read transaction, so the copy is the store as of one moment, whatever
//! the writer does meanwhile, and compacted on the way. Beside it go
//! `config.toml`, `seal.key` (without it the sealed quarantine names can't
//! be read) and `auth.json` (the admin password hash, sessions and API
//! keys). The search index isn't copied: it's rebuilt from the store.
//!
//! The update trust state (`bin/trust.json`) is left out on purpose: §12.2
//! keeps it out of every snapshot and backup, so restoring one can never
//! undo a key revocation.

use std::path::{Path, PathBuf};

use super::util::{
    Failure, Outcome, copy_new_private, create_new_private, failed, file_stamp, format_bytes,
    format_count, open_read_only, quick_check, refuse_during_update_rollback, schema_version,
    sync_dir, torrent_count, unix_now, vacuum_into,
};
use crate::config::Config;

/// What a backup wrote, for the summary.
#[derive(Debug)]
pub struct Report {
    pub dir: PathBuf,
    pub files: Vec<(String, u64)>,
    pub schema: i64,
    pub torrents: Option<u64>,
}

/// Backs up into a new folder `lodestar-backup-<UTC time>` under `to`
/// (default: `<data>/backups`).
pub fn run(config: &Config, config_file: Option<&Path>, to: Option<&Path>) -> Outcome {
    let report = backup(config, config_file, to, unix_now())?;
    println!("Backup written to {}", report.dir.display());
    for (name, bytes) in &report.files {
        println!("  {name:<14} {}", format_bytes(*bytes));
    }
    let torrents = report.torrents.map_or_else(|| "no".into(), format_count);
    println!(
        "The store holds {torrents} torrents (schema v{}).",
        report.schema
    );
    println!(
        "The search index isn't included. After a restore, rebuild it with `lodestar reindex`."
    );
    println!(
        "The folder holds secrets (seal.key, auth.json): keep it private. Its README.txt has the \
         restore steps."
    );
    Ok(())
}

pub fn backup(
    config: &Config,
    config_file: Option<&Path>,
    to: Option<&Path>,
    now: i64,
) -> Outcome<Report> {
    let data = &config.data_dir;
    refuse_during_update_rollback(data)?;
    let db = config.db_path();
    // Opened before anything is created, so a missing store fails cleanly.
    let conn = open_read_only(&db)?;
    let parent = match to {
        Some(p) => p.to_path_buf(),
        None => data.join("backups"),
    };
    std::fs::create_dir_all(&parent).map_err(|e| {
        Failure::Failed(format!(
            "creating {}: {}",
            parent.display(),
            super::util::explain_denied(&e)
        ))
    })?;
    let parent = std::fs::canonicalize(&parent)
        .map_err(failed(format!("resolving {}", parent.display())))?;
    let dir = parent.join(format!("lodestar-backup-{}", file_stamp(now)));
    // A new folder every time: an existing one is never written into.
    create_private_dir(&dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => {
            Failure::Refused(format!("{} exists already", dir.display()))
        }
        _ => Failure::Failed(format!(
            "creating {}: {}",
            dir.display(),
            super::util::explain_denied(&e)
        )),
    })?;

    let mut files = Vec::new();
    let copy = dir.join("lodestar.db");
    vacuum_into(&conn, &copy)?;
    drop(conn);
    let check = open_read_only(&copy)?;
    quick_check(&check).map_err(|e| {
        Failure::Failed(format!("the copy {} fails its check: {e}", copy.display()))
    })?;
    let schema = schema_version(&check).map_err(failed("reading the copy's schema"))?;
    let torrents = torrent_count(&check).map_err(failed("counting the copy's torrents"))?;
    drop(check);
    restrict(&copy);
    files.push(("lodestar.db".to_string(), super::util::file_len(&copy)));

    let extra = [
        ("config.toml", config_file.map(Path::to_path_buf)),
        ("seal.key", Some(data.join("seal.key"))),
        ("auth.json", Some(data.join("auth.json"))),
    ];
    for (name, source) in extra {
        let Some(source) = source.filter(|p| p.is_file()) else {
            continue;
        };
        let n = copy_new_private(&source, &dir.join(name))
            .map_err(failed(format!("copying {}", source.display())))?;
        files.push((name.to_string(), n));
    }
    write_readme(&dir, &files, now)?;
    sync_dir(&dir);
    sync_dir(&parent);
    Ok(Report {
        dir,
        files,
        schema,
        torrents,
    })
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// The store copy holds sealed names and settings: owner-only, like the
/// other files.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn write_readme(dir: &Path, files: &[(String, u64)], now: i64) -> Outcome {
    use std::io::Write;
    let path = dir.join("README.txt");
    let mut text = format!(
        "Lodestar backup, taken {} by lodestar {}.\n\n",
        super::util::format_time(now),
        env!("CARGO_PKG_VERSION")
    );
    for (name, _) in files {
        text.push_str(&format!("  {name}\n"));
    }
    text.push_str(
        "\nThe search index isn't included: it's rebuilt from lodestar.db.\n\
         The update trust state (bin/trust.json) isn't included either, so a restore can\n\
         never undo a signing-key revocation.\n\n\
         To restore:\n\
         1. Stop Lodestar.\n\
         2. In the data folder, delete lodestar.db-wal and lodestar.db-shm if they exist.\n\
         3. Copy these files into the data folder, replacing the ones there.\n\
         4. Run `lodestar reindex`. The old search index doesn't match the restored store\n\
            and would show the wrong torrents; this rebuilds it at the next start.\n\
         5. Start Lodestar.\n\
         6. Sign out all sessions and create new API keys: auth.json brings back the\n\
            sessions and keys that existed when this backup was taken, including any\n\
            you have revoked since.\n\n\
         seal.key and auth.json are secrets: keep this folder private.\n",
    );
    let mut f = create_new_private(&path).map_err(failed(format!("writing {}", path.display())))?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(failed(format!("writing {}", path.display())))
}
