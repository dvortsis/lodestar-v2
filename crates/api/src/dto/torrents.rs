//! `/torrents/{hash}` and `/torrents/{hash}/files` (§8, §9).

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// `GET /torrents/{hash}`: the torrent summary (`docs/metrics.md` §7.1),
/// its swarm section (§7.2) and the first 500 files.
///
/// The swarm fields are `null` until swarm health is served (M4): null is
/// "never scraped", never 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TorrentDetail {
    /// Null = never scraped.
    #[schema(required = true)]
    pub completed: Option<u32>,
    /// BEP 33 estimate of peers (always approximate); null = no estimate.
    #[schema(required = true)]
    pub dht_peers: Option<u32>,
    /// BEP 33 estimate of seeds (always approximate); null = no estimate.
    #[schema(required = true)]
    pub dht_seeds: Option<u32>,
    pub discovered_at: i64,
    /// The first files, in the torrent's order; the rest via `/files`.
    pub file_list: Vec<FileEntry>,
    /// Files in all.
    pub file_list_total: usize,
    pub files: u64,
    pub flags: Vec<String>,
    /// 40 hex digits.
    pub hash: String,
    /// 64 hex digits; null for a v1-only torrent.
    #[schema(required = true)]
    pub hash_v2: Option<String>,
    pub id: i64,
    /// Null = never scraped.
    #[schema(required = true)]
    pub last_scrape_at: Option<i64>,
    /// Null = never scraped.
    #[schema(required = true)]
    pub leechers: Option<u32>,
    pub magnet: String,
    pub metadata_at: i64,
    pub name: String,
    pub piece_len: u64,
    /// Null = never scraped.
    #[schema(required = true)]
    pub seeders: Option<u32>,
    pub size: u64,
    /// 14 daily and 26 weekly samples, newest first; null = no history
    /// kept.
    #[schema(required = true)]
    pub swarm_history: Option<SwarmHistory>,
    /// Seeders per day over the last week; null = fewer than 3 daily
    /// samples.
    #[schema(required = true)]
    pub swarm_trend: Option<f64>,
    /// Trackers that answered in the last answered round; null = never
    /// scraped.
    #[schema(required = true)]
    pub trackers_checked: Option<u32>,
    /// Of those, trackers that reported anyone; null = never scraped.
    #[schema(required = true)]
    pub trackers_reporting: Option<u32>,
}

/// A torrent's swarm history (`docs/metrics.md` §7.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SwarmHistory {
    pub daily: Vec<SwarmSample>,
    pub weekly: Vec<SwarmSample>,
}

/// One day or week of swarm history. Counts saturate at 65,534; null is
/// a gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SwarmSample {
    /// The start of the UTC day, or of the week (epoch day / 7: weeks
    /// start on Thursday).
    pub at: i64,
    #[schema(required = true)]
    pub leechers: Option<u32>,
    #[schema(required = true)]
    pub seeders: Option<u32>,
}

/// One file of a torrent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FileEntry {
    /// Any of `padding`, `hidden`, `executable`, `symlink`.
    pub attrs: Vec<String>,
    /// `/`-separated, decoded.
    pub path: String,
    pub size: u64,
}

/// `GET /torrents/{hash}/files`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct FilesQuery {
    /// Files to skip.
    pub offset: usize,
    /// Files per page: 1–1000, 1000 by default.
    pub limit: Option<usize>,
}

/// `GET /torrents/{hash}/files`: a page of the file list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FilesPage {
    pub files: Vec<FileEntry>,
    pub offset: usize,
    /// Files in all.
    pub total: usize,
}
