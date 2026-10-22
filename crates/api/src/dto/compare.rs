//! `/torrents/{hash}/siblings` ("Related", §10.5) and `/compare`
//! (§10.6).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::discover::HitGroup;
use super::search::CompositionShare;

/// `GET /torrents/{hash}/siblings`: the torrents related to one, in three
/// groups. Each is either available (possibly empty) or unavailable with
/// a reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SiblingsResponse {
    /// 40 hex digits: the torrent asked about.
    pub hash: String,
    /// Identical-payload clones: the same content under another hash.
    pub identical_payload: HitGroup,
    /// What other versions were matched on; null when the torrent has no
    /// parsed title.
    #[schema(required = true)]
    pub identity: Option<TitleIdentity>,
    /// Other versions of the same work (other releases, qualities,
    /// groups), best first.
    pub other_versions: HitGroup,
    /// Other torrents containing one of this torrent's files.
    pub same_file: HitGroup,
}

/// A work's title identity (§16, B13): its normalized title plus the
/// year, season and episode spans the release names, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TitleIdentity {
    /// Absolute episode spans (anime numbering).
    pub absolute: Vec<String>,
    /// Episode spans (`2`, `1-10`).
    pub episodes: Vec<String>,
    /// Season spans.
    pub seasons: Vec<String>,
    /// The title's words as search tokenizes them, lowercased and
    /// folded.
    pub title_words: Vec<String>,
    /// Year spans (`2008`, `1990-1999`).
    pub years: Vec<String>,
}

/// `POST /compare`'s body.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CompareRequest {
    /// 2–5 distinct 40-character hex info hashes, in column order.
    pub hashes: Vec<String>,
}

/// `POST /compare`: 2–5 torrents side by side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CompareResponse {
    /// For each parsed attribute any torrent has (the release fields:
    /// `title`, `year`, `res`, `vcodec`, `group`, …), whether the values
    /// differ across the set. A torrent without the attribute counts as
    /// a different value.
    pub differs: BTreeMap<String, bool>,
    /// The file lists compared by path and size.
    pub file_diff: FileDiff,
    /// The shared scale for the composition bars: the largest `size` of
    /// the set, in bytes.
    pub scale_bytes: u64,
    /// One per hash, in the request's order.
    pub torrents: Vec<CompareTorrent>,
}

/// One torrent's column in Compare.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CompareTorrent {
    /// Every attribute in `differs`: this torrent's value, or null when
    /// it has none. Lists are comma-joined.
    pub attributes: BTreeMap<String, Option<String>>,
    /// As in a search hit; empty before classification.
    pub composition: Vec<CompositionShare>,
    /// As in a search hit; null = not classified.
    #[schema(required = true)]
    pub content_type: Option<String>,
    pub discovered_at: i64,
    pub files: u64,
    /// 40 hex digits.
    pub hash: String,
    /// Null = never scraped.
    #[schema(required = true)]
    pub leechers: Option<u32>,
    pub name: String,
    /// Null = never scraped.
    #[schema(required = true)]
    pub seeders: Option<u32>,
    pub size: u64,
    /// The effective title; null = not resolved.
    #[schema(required = true)]
    pub title: Option<String>,
    /// Null = never scraped.
    #[schema(required = true)]
    pub trackers_checked: Option<u32>,
    /// Null = never scraped.
    #[schema(required = true)]
    pub trackers_reporting: Option<u32>,
}

/// The file lists compared: files match when their path (inside the
/// torrent, `/`-separated, decoded) and size are equal. Padding files
/// are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FileDiff {
    /// Files every torrent has, in the first torrent's order.
    pub common: FileList,
    /// Per torrent, in the request's order: files no other torrent has.
    /// With three or more torrents, a file some but not all have is in
    /// neither list.
    pub only: Vec<FileList>,
}

/// A bounded list of files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FileList {
    /// Files in the list in all, listed or not.
    pub count: u64,
    /// At most 2,000, in the torrent's order.
    pub files: Vec<DiffFile>,
    /// `count` is more than `files` lists.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiffFile {
    /// `/`-separated, decoded.
    pub path: String,
    pub size: u64,
}
