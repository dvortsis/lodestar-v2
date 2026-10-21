//! Importing a bitmagnet database (architecture §14, the M2 add-on).
//!
//! bitmagnet fetched these torrents' metadata from the DHT itself, so they
//! go straight into `torrents`, flagged `imported`, with bitmagnet's
//! `created_at` as their discovery time. bitmagnet keeps no file rows for
//! torrents over its `save_files_threshold` (and none at all for some), so
//! those are flagged `files_partial`; the server re-fetches their info
//! dicts from the DHT and fills the files in (§16).
//!
//! The source is never written: the session is read-only, so PostgreSQL
//! itself refuses writes. The import is batched and resumable: it walks
//! `torrents` in `info_hash` order and records how far it got, and
//! re-importing what's already stored changes nothing.

use std::path::{Path, PathBuf};

use lodestar_metainfo::{
    File, FileAttrs, FileHashes, Info, InfoHash, MetaVersion, Text, TextEncoding,
};
use lodestar_store::{NewTorrent, SourceId, Store, flags};
use tokio_postgres::{Client, NoTls};

pub const PLUGIN_ID: &str = "bitmagnet";

#[derive(Debug)]
pub enum ImportError {
    Postgres(tokio_postgres::Error),
    Store(lodestar_store::Error),
    Io(std::io::Error),
    /// The database doesn't look like bitmagnet's.
    Schema(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Postgres(e) => write!(f, "postgres: {e}"),
            ImportError::Store(e) => e.fmt(f),
            ImportError::Io(e) => write!(f, "cursor file: {e}"),
            ImportError::Schema(why) => write!(f, "not a bitmagnet database: {why}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<tokio_postgres::Error> for ImportError {
    fn from(e: tokio_postgres::Error) -> Self {
        ImportError::Postgres(e)
    }
}

impl From<lodestar_store::Error> for ImportError {
    fn from(e: lodestar_store::Error) -> Self {
        ImportError::Store(e)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Torrents per batch (one store transaction each).
    pub batch: usize,
    /// Where to remember progress, so a rerun continues where it stopped.
    pub cursor: Option<PathBuf>,
    /// Stop after this many torrents (for trying it out).
    pub limit: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            batch: 1000,
            cursor: None,
            limit: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Torrents read from bitmagnet.
    pub read: u64,
    /// Newly stored.
    pub new: u64,
    /// Already stored (from an earlier run or our own crawler).
    pub existing: u64,
    /// Stored with an incomplete file list, to be re-fetched.
    pub partial: u64,
    /// Rows that couldn't be imported (malformed).
    pub skipped: u64,
}

/// One `torrents` row with its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub info_hash: Vec<u8>,
    pub name: String,
    pub size: i64,
    pub private: bool,
    /// `no_info`, `single`, `multi` or `over_threshold`.
    pub files_status: String,
    pub files_count: Option<i32>,
    pub piece_length: Option<i64>,
    pub pieces_len: Option<i32>,
    pub created_at: i64,
    /// `(index, path, size)`, in index order.
    pub files: Vec<(i32, String, i64)>,
}

/// An `Info` built from a row, and whether its file list is incomplete.
/// `None` for a row that can't be a torrent.
pub fn to_info(row: &Row) -> Option<(Info, bool)> {
    let hash: [u8; 20] = row.info_hash.as_slice().try_into().ok()?;
    if row.name.is_empty() || row.size < 0 {
        return None;
    }
    let file = |path: Vec<Vec<u8>>, size: u64| File {
        path,
        size,
        attrs: FileAttrs::empty(),
        symlink_target: None,
        pieces_root: None,
        hashes: FileHashes::default(),
    };
    let single = row.files_status == "single";
    let mut files = Vec::new();
    if single {
        files.push(file(vec![row.name.as_bytes().to_vec()], row.size as u64));
    } else {
        for (_, path, size) in &row.files {
            let segments: Vec<Vec<u8>> = path
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| s.as_bytes().to_vec())
                .collect();
            if segments.is_empty() || *size < 0 {
                continue;
            }
            files.push(file(segments, *size as u64));
        }
    }
    let partial = !single
        && (files.is_empty()
            || row.files_status != "multi"
            || row
                .files_count
                .is_some_and(|n| (files.len() as i64) < i64::from(n)));
    let total_size = if partial || files.is_empty() {
        row.size as u64
    } else {
        files.iter().map(|f| f.size).sum()
    };
    let info = Info {
        v1: Some(InfoHash(hash)),
        v2: None,
        version: MetaVersion::V1,
        name: Text {
            raw: row.name.as_bytes().to_vec(),
            encoding: TextEncoding::UTF8,
        },
        piece_length: row.piece_length.unwrap_or(0).max(0) as u64,
        piece_count: (row.pieces_len.unwrap_or(0).max(0) / 20) as u64,
        total_size,
        private: row.private,
        source: None,
        single_file: single,
        files,
        path_encoding: TextEncoding::UTF8,
        extra: None,
        info_size: 0,
        warnings: Vec::new(),
    };
    Some((info, partial))
}

async fn has_table(client: &Client, name: &str) -> Result<bool, ImportError> {
    let row = client
        .query_one("SELECT to_regclass($1)::text", &[&name])
        .await?;
    Ok(row.get::<_, Option<String>>(0).is_some())
}

async fn has_column(client: &Client, table: &str, column: &str) -> Result<bool, ImportError> {
    let row = client
        .query_one(
            "SELECT count(*) FROM information_schema.columns
             WHERE table_name = $1 AND column_name = $2 AND table_schema = ANY(current_schemas(false))",
            &[&table, &column],
        )
        .await?;
    Ok(row.get::<_, i64>(0) > 0)
}

fn read_cursor(path: &Path) -> Result<Vec<u8>, ImportError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let t = text.trim();
            if t.len() % 2 != 0 {
                return Err(ImportError::Io(std::io::Error::other("bad cursor")));
            }
            (0..t.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&t[i..i + 2], 16))
                .collect::<Result<_, _>>()
                .map_err(|_| ImportError::Io(std::io::Error::other("bad cursor")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(ImportError::Io(e)),
    }
}

fn write_cursor(path: &Path, hash: &[u8]) -> Result<(), ImportError> {
    let tmp = path.with_extension("tmp");
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&tmp, hex).map_err(ImportError::Io)?;
    std::fs::rename(&tmp, path).map_err(ImportError::Io)
}

/// Imports everything after the saved cursor into `store` as `source`.
/// `progress` is called after each batch with the totals so far.
pub async fn import(
    url: &str,
    store: &mut Store,
    source: SourceId,
    opts: &Options,
    mut progress: impl FnMut(&Report),
) -> Result<Report, ImportError> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    let conn_task = tokio::spawn(connection);
    // Every transaction in this session is read-only: PostgreSQL refuses
    // any write, so the import can't change the source.
    client
        .batch_execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
        .await?;
    for t in ["torrents", "torrent_files"] {
        if !has_table(&client, t).await? {
            return Err(ImportError::Schema(format!("no {t} table")));
        }
    }
    if !has_column(&client, "torrents", "files_status").await? {
        return Err(ImportError::Schema(
            "torrents.files_status missing (bitmagnet too old)".into(),
        ));
    }
    // Newer bitmagnets keep pieces in torrent_pieces; older ones have no
    // piece data we can read cheaply.
    let pieces = if has_table(&client, "torrent_pieces").await? {
        (
            "p.piece_length, length(p.pieces)",
            "LEFT JOIN torrent_pieces p ON p.info_hash = t.info_hash",
        )
    } else {
        ("NULL::bigint, NULL::int", "")
    };
    let files_count = if has_column(&client, "torrents", "files_count").await? {
        "t.files_count"
    } else {
        "NULL::int"
    };
    let torrents_sql = format!(
        "SELECT t.info_hash, t.name, t.size, t.private, t.files_status::text, {files_count},
                {}, extract(epoch from t.created_at)::bigint
         FROM torrents t {}
         WHERE t.info_hash > $1 ORDER BY t.info_hash LIMIT $2",
        pieces.0, pieces.1
    );
    let torrents = client.prepare(&torrents_sql).await?;
    let files = client
        .prepare(
            "SELECT info_hash, \"index\", path, size FROM torrent_files
             WHERE info_hash = ANY($1) ORDER BY info_hash, \"index\"",
        )
        .await?;

    let mut cursor = match &opts.cursor {
        Some(p) => read_cursor(p)?,
        None => Vec::new(),
    };
    let mut report = Report::default();
    loop {
        let mut batch = opts.batch.max(1) as i64;
        if let Some(limit) = opts.limit {
            batch = batch.min(limit.saturating_sub(report.read) as i64);
            if batch == 0 {
                break;
            }
        }
        let rows = client.query(&torrents, &[&cursor, &batch]).await?;
        if rows.is_empty() {
            break;
        }
        let mut batch_rows: Vec<Row> = rows
            .iter()
            .map(|r| Row {
                info_hash: r.get(0),
                name: r.get(1),
                size: r.get(2),
                private: r.get(3),
                files_status: r.get(4),
                files_count: r.get(5),
                piece_length: r.get(6),
                pieces_len: r.get(7),
                created_at: r.get::<_, Option<i64>>(8).unwrap_or(0),
                files: Vec::new(),
            })
            .collect();
        let hashes: Vec<&[u8]> = batch_rows.iter().map(|r| r.info_hash.as_slice()).collect();
        let file_rows = client.query(&files, &[&hashes]).await?;
        let mut j = 0;
        for row in &mut batch_rows {
            while j < file_rows.len() && file_rows[j].get::<_, &[u8]>(0) < row.info_hash.as_slice()
            {
                j += 1;
            }
            while j < file_rows.len() && file_rows[j].get::<_, &[u8]>(0) == row.info_hash.as_slice()
            {
                let f = &file_rows[j];
                row.files.push((f.get(1), f.get(2), f.get(3)));
                j += 1;
            }
        }
        cursor = batch_rows.last().expect("non-empty").info_hash.clone();
        report.read += batch_rows.len() as u64;

        let converted: Vec<(Info, bool, i64)> = batch_rows
            .iter()
            .filter_map(|r| to_info(r).map(|(info, partial)| (info, partial, r.created_at)))
            .collect();
        report.skipped += (batch_rows.len() - converted.len()) as u64;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let new_torrents: Vec<NewTorrent<'_>> = converted
            .iter()
            .map(|(info, partial, created)| NewTorrent {
                info,
                raw_info: None,
                discovered_at: *created,
                metadata_at: now,
                extra_flags: flags::IMPORTED | if *partial { flags::FILES_PARTIAL } else { 0 },
                source,
            })
            .collect();
        let inserted = store.insert(&new_torrents)?;
        for ((_, partial, _), ins) in converted.iter().zip(&inserted) {
            match ins {
                lodestar_store::Inserted::New(_) => {
                    report.new += 1;
                    report.partial += u64::from(*partial);
                }
                lodestar_store::Inserted::Existing(_) => report.existing += 1,
            }
        }
        if let Some(p) = &opts.cursor {
            write_cursor(p, &cursor)?;
        }
        progress(&report);
    }
    drop(client);
    let _ = conn_task.await;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, files: Vec<(i32, &str, i64)>) -> Row {
        Row {
            info_hash: vec![7; 20],
            name: "Some.Show.S01".into(),
            size: 3000,
            private: false,
            files_status: status.into(),
            files_count: Some(files.len() as i32),
            piece_length: Some(1 << 18),
            pieces_len: Some(60),
            created_at: 1_600_000_000,
            files: files
                .into_iter()
                .map(|(i, p, s)| (i, p.to_string(), s))
                .collect(),
        }
    }

    #[test]
    fn rows_become_infos() {
        let (info, partial) = to_info(&row(
            "multi",
            vec![(0, "E01.mkv", 1000), (1, "Subs/E01.srt", 2000)],
        ))
        .unwrap();
        assert!(!partial);
        assert_eq!(info.files.len(), 2);
        assert_eq!(
            info.files[1].path,
            vec![b"Subs".to_vec(), b"E01.srt".to_vec()]
        );
        assert_eq!(
            (info.total_size, info.piece_count, info.primary_hash()),
            (3000, 3, InfoHash([7; 20]))
        );

        let (info, partial) = to_info(&row("single", vec![])).unwrap();
        assert!(!partial && info.single_file);
        assert_eq!(info.files[0].path, vec![b"Some.Show.S01".to_vec()]);

        for status in ["over_threshold", "no_info"] {
            let (info, partial) = to_info(&row(status, vec![])).unwrap();
            assert!(partial, "{status}");
            assert!(info.files.is_empty());
            assert_eq!(info.total_size, 3000, "size from the torrent row");
        }
        // Fewer file rows than files_count: incomplete.
        let mut r = row("multi", vec![(0, "a", 1)]);
        r.files_count = Some(5);
        assert!(to_info(&r).unwrap().1);
    }

    #[test]
    fn malformed_rows_are_skipped() {
        let mut r = row("single", vec![]);
        r.info_hash = vec![1; 32];
        assert!(to_info(&r).is_none());
        let mut r = row("single", vec![]);
        r.name.clear();
        assert!(to_info(&r).is_none());
        // Empty paths are dropped, not fatal.
        let r = row("multi", vec![(0, "/", 1), (1, "ok.bin", 2)]);
        let (info, partial) = to_info(&r).unwrap();
        assert_eq!(info.files.len(), 1);
        assert!(partial, "a file was lost, so the list is incomplete");
    }
}
