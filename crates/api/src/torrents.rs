//! `/torrents/{hash}`: one torrent's details and its file list. Display
//! data comes from SQLite, not the index (§8).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lodestar_metainfo::{FileAttrs, InfoHash, Text};
use lodestar_search::flag_names;
use lodestar_store::{FileTree, TorrentRow};
use utoipa::OpenApi;

use crate::dto::search::SearchHit;
use crate::dto::torrents::{FileEntry, FilesPage, FilesQuery, TorrentDetail};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Path, Query};
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(detail, files))]
pub(crate) struct ApiDoc;

/// Files in the detail response; more via `/files` (§9).
pub const DETAIL_FILES: usize = 500;
const MAX_FILES_PAGE: usize = 1000;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/torrents/{hash}", get(detail))
            .route("/torrents/{hash}/files", get(files)),
    )
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn parse_hash(s: &str) -> ApiResult<InfoHash> {
    let bad = || {
        ApiError::bad_request("bad_hash", "Expected a 40-character hex info hash.").field("hash")
    };
    if s.len() != 40 {
        return Err(bad());
    }
    let mut out = [0u8; 20];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16).map_err(|_| bad())?;
    }
    Ok(InfoHash(out))
}

fn flags(row: &TorrentRow) -> Vec<String> {
    flag_names(row.flags).map(str::to_string).collect()
}

/// A search result row. The swarm fields stay null (never scraped) until
/// swarm health is served (M4); the classification fields are filled from
/// the stored enrichment by `search::enrich`.
pub(crate) fn hit(row: &TorrentRow, score: f32, matched_files: Option<Vec<u64>>) -> SearchHit {
    SearchHit {
        adult_kind: None,
        completed: None,
        composition: Vec::new(),
        content_type: None,
        discovered_at: row.discovered_at,
        files: row.file_count,
        flags: flags(row),
        hash: hex(&row.info_hash.0),
        hash_v2: row.info_hash_v2.map(|h| hex(&h.0)),
        id: row.id,
        junk_reasons: Vec::new(),
        junk_score: None,
        last_scrape_at: None,
        leechers: None,
        matched_files,
        metadata_at: row.metadata_at,
        name: row.name.clone(),
        piece_len: row.piece_len,
        resolved_via: None,
        // As `json!` widened it.
        score: f64::from(score),
        seeders: None,
        size: row.size,
        title: None,
        trackers_checked: None,
        trackers_reporting: None,
    }
}

fn file_entry(tree: &FileTree, f: &lodestar_metainfo::File) -> FileEntry {
    let path: Vec<String> = f
        .path
        .iter()
        .map(|seg| {
            Text {
                raw: seg.clone(),
                encoding: tree.encoding,
            }
            .decode()
            .into_owned()
        })
        .collect();
    let mut attrs = Vec::new();
    for (bit, name) in [
        (FileAttrs::PADDING, "padding"),
        (FileAttrs::HIDDEN, "hidden"),
        (FileAttrs::EXECUTABLE, "executable"),
        (FileAttrs::SYMLINK, "symlink"),
    ] {
        if f.attrs.contains(bit) {
            attrs.push(name.to_string());
        }
    }
    FileEntry {
        attrs,
        path: path.join("/"),
        size: f.size,
    }
}

/// Reads a torrent and its files by any swarm hash.
fn load(s: &ApiState, hash: &str) -> ApiResult<(TorrentRow, FileTree)> {
    let hash = parse_hash(hash)?;
    let mut store = s.reader.lock().expect("reader lock");
    let not_found = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "torrent_not_found",
            "There's no torrent with that hash.",
        )
    };
    let id = store.find(&hash).map_err(internal)?.ok_or_else(not_found)?;
    let row = store.torrent(id).map_err(internal)?.ok_or_else(not_found)?;
    let tree = store.files(id).map_err(internal)?.ok_or_else(not_found)?;
    Ok((row, tree))
}

/// The detail view: the summary, the swarm section (null until swarm
/// health is served) and the first files.
fn detail_of(row: &TorrentRow, tree: &FileTree) -> TorrentDetail {
    TorrentDetail {
        completed: None,
        dht_peers: None,
        dht_seeds: None,
        discovered_at: row.discovered_at,
        file_list: tree
            .files
            .iter()
            .take(DETAIL_FILES)
            .map(|f| file_entry(tree, f))
            .collect(),
        file_list_total: tree.files.len(),
        files: row.file_count,
        flags: flags(row),
        hash: hex(&row.info_hash.0),
        hash_v2: row.info_hash_v2.map(|h| hex(&h.0)),
        id: row.id,
        last_scrape_at: None,
        leechers: None,
        magnet: format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            hex(&row.info_hash.0),
            percent_encode(&row.name)
        ),
        metadata_at: row.metadata_at,
        name: row.name.clone(),
        piece_len: row.piece_len,
        seeders: None,
        size: row.size,
        swarm_history: None,
        swarm_trend: None,
        trackers_checked: None,
        trackers_reporting: None,
    }
}

/// One torrent by any of its swarm hashes.
#[utoipa::path(
    operation_id = "getTorrent",
    get,
    path = "/torrents/{hash}",
    tag = "torrents",
    params(("hash" = String, Path, description = "A 40-character hex info hash.")),
    responses((status = 200, body = TorrentDetail))
)]
pub(crate) async fn detail(
    State(s): State<ApiState>,
    Path(hash): Path<String>,
) -> ApiResult<Json<TorrentDetail>> {
    tokio::task::spawn_blocking(move || {
        let (row, tree) = load(&s, &hash)?;
        Ok(Json(detail_of(&row, &tree)))
    })
    .await
    .map_err(internal)?
}

/// A page of a torrent's file list.
#[utoipa::path(
    operation_id = "listTorrentFiles",
    get,
    path = "/torrents/{hash}/files",
    tag = "torrents",
    params(
        ("hash" = String, Path, description = "A 40-character hex info hash."),
        FilesQuery,
    ),
    responses((status = 200, body = FilesPage))
)]
pub(crate) async fn files(
    State(s): State<ApiState>,
    Path(hash): Path<String>,
    Query(p): Query<FilesQuery>,
) -> ApiResult<Json<FilesPage>> {
    tokio::task::spawn_blocking(move || {
        let (_, tree) = load(&s, &hash)?;
        let limit = p.limit.unwrap_or(MAX_FILES_PAGE).clamp(1, MAX_FILES_PAGE);
        let files = tree
            .files
            .iter()
            .skip(p.offset)
            .take(limit)
            .map(|f| file_entry(&tree, f))
            .collect();
        Ok(Json(FilesPage {
            files,
            offset: p.offset,
            total: tree.files.len(),
        }))
    })
    .await
    .map_err(internal)?
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
