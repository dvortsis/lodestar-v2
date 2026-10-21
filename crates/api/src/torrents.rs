//! `/torrents/{hash}`: one torrent's details and its file list. Display
//! data comes from SQLite, not the index (§8).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use lodestar_metainfo::{FileAttrs, InfoHash, Text};
use lodestar_search::flag_names;
use lodestar_store::{FileTree, TorrentRow};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ApiError, ApiResult, ApiState, internal};

/// Files in the detail response; more via `/files` (§9).
pub const DETAIL_FILES: usize = 500;
const MAX_FILES_PAGE: usize = 1000;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn parse_hash(s: &str) -> ApiResult<InfoHash> {
    let bad = || ApiError::new(StatusCode::BAD_REQUEST, "expected a 40-hex info hash");
    if s.len() != 40 {
        return Err(bad());
    }
    let mut out = [0u8; 20];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16).map_err(|_| bad())?;
    }
    Ok(InfoHash(out))
}

/// The fields a result row shows.
pub fn summary(row: &TorrentRow) -> Value {
    json!({
        "id": row.id,
        "hash": hex(&row.info_hash.0),
        "hash_v2": row.info_hash_v2.map(|h| hex(&h.0)),
        "name": row.name,
        "size": row.size,
        "files": row.file_count,
        "piece_len": row.piece_len,
        "discovered_at": row.discovered_at,
        "metadata_at": row.metadata_at,
        "flags": flag_names(row.flags).collect::<Vec<_>>(),
    })
}

fn file_json(tree: &FileTree, f: &lodestar_metainfo::File) -> Value {
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
            attrs.push(name);
        }
    }
    json!({ "path": path.join("/"), "size": f.size, "attrs": attrs })
}

/// Reads a torrent and its files by any swarm hash.
fn load(s: &ApiState, hash: &str) -> ApiResult<(TorrentRow, FileTree)> {
    let hash = parse_hash(hash)?;
    let mut store = s.reader.lock().expect("reader lock");
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "no such torrent");
    let id = store.find(&hash).map_err(internal)?.ok_or_else(not_found)?;
    let row = store.torrent(id).map_err(internal)?.ok_or_else(not_found)?;
    let tree = store.files(id).map_err(internal)?.ok_or_else(not_found)?;
    Ok((row, tree))
}

pub async fn detail(State(s): State<ApiState>, Path(hash): Path<String>) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let (row, tree) = load(&s, &hash)?;
        let mut v = summary(&row);
        let shown: Vec<Value> = tree
            .files
            .iter()
            .take(DETAIL_FILES)
            .map(|f| file_json(&tree, f))
            .collect();
        v["file_list"] = json!(shown);
        v["file_list_total"] = json!(tree.files.len());
        v["magnet"] = json!(format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            hex(&row.info_hash.0),
            percent_encode(&row.name)
        ));
        Ok(Json(v))
    })
    .await
    .map_err(internal)?
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct FilesParams {
    offset: usize,
    limit: Option<usize>,
}

pub async fn files(
    State(s): State<ApiState>,
    Path(hash): Path<String>,
    Query(p): Query<FilesParams>,
) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let (_, tree) = load(&s, &hash)?;
        let limit = p.limit.unwrap_or(MAX_FILES_PAGE).clamp(1, MAX_FILES_PAGE);
        let page: Vec<Value> = tree
            .files
            .iter()
            .skip(p.offset)
            .take(limit)
            .map(|f| file_json(&tree, f))
            .collect();
        Ok(Json(
            json!({ "total": tree.files.len(), "offset": p.offset, "files": page }),
        ))
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
