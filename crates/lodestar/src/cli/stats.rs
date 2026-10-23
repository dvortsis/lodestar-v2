//! `lodestar stats [--json]`: counts, sizes and the index's state. It only
//! reads (a read-only store connection and directory listings), so it's
//! safe while the server runs.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::rollback::snapshots;
use super::util::{
    Outcome, dir_bytes, failed, file_len, format_bytes, format_count, open_read_only,
    refuse_during_update_rollback, schema_version, side_files, torrent_count,
};
use crate::config::Config;
use crate::fsdetect::{self, FsReport};

#[derive(Debug, Serialize)]
pub struct Stats {
    pub data_dir: PathBuf,
    pub filesystem: FsReport,
    pub filesystem_warning: Option<String>,
    pub store: StoreStats,
    pub index: IndexStats,
    pub snapshots: SnapshotStats,
    /// Whether a server answers `/healthz` on the configured address.
    pub server_running: bool,
}

#[derive(Debug, Serialize)]
pub struct StoreStats {
    pub path: PathBuf,
    pub schema: i64,
    pub torrents: Option<u64>,
    pub file_bytes: u64,
    pub wal_bytes: u64,
    /// Free pages inside the file, which `lodestar vacuum` returns.
    pub free_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct IndexStats {
    pub root: PathBuf,
    /// This build's directory name, `v{schema}-t{tokenizer}`.
    pub version: String,
    pub state: IndexState,
    pub bytes: u64,
    pub directories: Vec<IndexDir>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    /// This build's index is complete.
    Ready,
    /// This build's index is being built (an older one may serve).
    Building,
    /// There's no index for this build: it's built at the next start.
    Missing,
}

#[derive(Debug, Serialize)]
pub struct IndexDir {
    pub name: String,
    pub kind: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct SnapshotStats {
    pub count: usize,
    pub bytes: u64,
}

pub fn run(config: &Config, json: bool) -> Outcome {
    let running = crate::healthcheck(crate::settings::network::effective_bind(config)).is_ok();
    let stats = collect(config, running)?;
    if json {
        let text = serde_json::to_string_pretty(&stats).map_err(failed("encoding"))?;
        println!("{text}");
    } else {
        print_text(&stats);
    }
    Ok(())
}

pub fn collect(config: &Config, server_running: bool) -> Outcome<Stats> {
    refuse_during_update_rollback(&config.data_dir)?;
    let db = config.db_path();
    let conn = open_read_only(&db)?;
    let pragma = |name: &str| -> Outcome<u64> {
        conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0))
            .map(|v| v.max(0) as u64)
            .map_err(failed(format!("reading {name}")))
    };
    let free_bytes = pragma("freelist_count")? * pragma("page_size")?;
    let store = StoreStats {
        schema: schema_version(&conn).map_err(failed("reading the schema version"))?,
        torrents: torrent_count(&conn).map_err(failed("counting torrents"))?,
        file_bytes: file_len(&db),
        wal_bytes: file_len(&side_files(&db)[0]),
        free_bytes,
        path: db.clone(),
    };
    drop(conn);
    let current = config.index_path();
    let root = current.parent().unwrap_or(&current).to_path_buf();
    let version = current
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let directories = index_dirs(&root);
    let has = |name: &str| directories.iter().any(|d| d.name == name);
    let state = if has(&version) {
        IndexState::Ready
    } else if has(&format!("{version}.building")) {
        IndexState::Building
    } else {
        IndexState::Missing
    };
    let snaps = snapshots(&db)?;
    let filesystem = fsdetect::detect(&config.data_dir);
    Ok(Stats {
        data_dir: config.data_dir.clone(),
        filesystem_warning: filesystem.warning(),
        filesystem,
        store,
        index: IndexStats {
            bytes: directories.iter().map(|d| d.bytes).sum(),
            root,
            version,
            state,
            directories,
        },
        snapshots: SnapshotStats {
            count: snaps.len(),
            bytes: snaps.iter().map(|s| s.bytes).sum(),
        },
        server_running,
    })
}

/// The index root's version directories, by name.
fn index_dirs(root: &Path) -> Vec<IndexDir> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<IndexDir> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let kind = if name.ends_with(".trash") {
                "trash"
            } else if name.ends_with(".building") {
                "building"
            } else {
                "complete"
            };
            Some(IndexDir {
                bytes: dir_bytes(&e.path()),
                name,
                kind,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn print_text(s: &Stats) {
    let count = |n: Option<u64>| n.map_or_else(|| "—".into(), format_count);
    println!("Data folder   {}", s.data_dir.display());
    let fs = s.filesystem.fs_type.as_deref().unwrap_or("unknown type");
    println!("Filesystem    {fs}");
    if let Some(w) = &s.filesystem_warning {
        println!("  Warning: {w}");
    }
    println!(
        "Server        {}",
        if s.server_running {
            "running"
        } else {
            "not answering"
        }
    );
    println!("Store         {}", s.store.path.display());
    println!("  Schema      v{}", s.store.schema);
    println!("  Torrents    {}", count(s.store.torrents));
    println!(
        "  Size        {} (+ {} write-ahead log), {} free inside",
        format_bytes(s.store.file_bytes),
        format_bytes(s.store.wal_bytes),
        format_bytes(s.store.free_bytes)
    );
    let state = match s.index.state {
        IndexState::Ready => "ready",
        IndexState::Building => "rebuilding",
        IndexState::Missing => "not built yet; built at the next start",
    };
    println!("Search index  {}", s.index.root.display());
    println!("  Version     {} ({state})", s.index.version);
    println!("  Size        {}", format_bytes(s.index.bytes));
    for d in &s.index.directories {
        println!("    {:<24} {:<9} {}", d.name, d.kind, format_bytes(d.bytes));
    }
    println!(
        "Snapshots     {} ({})",
        s.snapshots.count,
        format_bytes(s.snapshots.bytes)
    );
}
