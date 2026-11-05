//! `/torrents/{hash}` and `/torrents/{hash}/files` (§8, §9).

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// `GET /torrents/{hash}`: the torrent summary (`docs/metrics.md` §7.1),
/// its swarm section (§7.2) and the first 500 files.
///
/// A null swarm field is "never scraped" (or no estimate), never 0.
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
    /// The latest DHT sighting.
    pub last_seen_at: i64,
    /// Null = never scraped.
    #[schema(required = true)]
    pub leechers: Option<u32>,
    pub magnet: String,
    pub metadata_at: i64,
    pub name: String,
    pub piece_len: u64,
    /// Where it came from (§18.5, §18.8): "Found by: DHT · Nyaa".
    pub provenance: Provenance,
    /// Null = never scraped.
    #[schema(required = true)]
    pub seeders: Option<u32>,
    /// No tracker reported: the counts are the DHT's estimate (show ≈).
    pub seeders_approx: bool,
    pub size: u64,
    /// One tracker reports a swarm nothing else sees (§8): fake seeding.
    pub swarm_suspect: bool,
    /// 14 daily and 26 weekly samples, newest first; null = no history
    /// kept.
    #[schema(required = true)]
    pub swarm_history: Option<SwarmHistory>,
    /// Seeders per day over the last week; null = fewer than 3 daily
    /// samples.
    #[schema(required = true)]
    pub swarm_trend: Option<f64>,
    /// The trackers that reported this swarm in the last answered round.
    pub swarm_trackers: Vec<SwarmTracker>,
    /// Trackers that answered in the last answered round; null = never
    /// scraped.
    #[schema(required = true)]
    pub trackers_checked: Option<u32>,
    /// Of those, trackers that reported anyone; null = never scraped.
    #[schema(required = true)]
    pub trackers_reporting: Option<u32>,
}

/// A torrent's provenance (architecture §18.5, §18.6, §18.8).
///
/// **Private sources (§18.7):** a source with the account consent class
/// (Prowlarr, Jackett, private-tracker announces, your own download
/// clients), claims that came through a private source, and every
/// source of a `private_source` torrent are shown only to the signed-in
/// admin session. Any other caller (an API key, once these routes take
/// one) gets them left out (a public source with a private claim keeps
/// its name, with `claim` null) and `withheld` set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "network": "bt",
    "sources": [
        {"claim": null, "first": true, "first_seen_at": 1792677600, "id": 0,
         "name": "Mainline DHT", "plugin": "dht", "private": false},
        {"claim": {"file_count": {"claimed": 3, "verified": 4}, "matches": false,
                   "name": null, "size": null},
         "first": false, "first_seen_at": null, "id": 2, "name": "Nyaa",
         "plugin": "nyaa", "private": false}
    ],
    "verified": true,
    "withheld": false
}))]
pub struct Provenance {
    /// The network tier (§18.6): `bt` for every stored torrent.
    pub network: String,
    /// The sources that found it: the first one first, then by id.
    /// Empty for a torrent stored before provenance was recorded.
    pub sources: Vec<ProvenanceSource>,
    /// It has verified metadata (§18.5). Always true here: a torrent's
    /// detail is its BEP 9 info dictionary.
    pub verified: bool,
    /// Sources or claims were left out because they're private and this
    /// caller isn't the admin session.
    pub withheld: bool,
}

/// One source that found a torrent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProvenanceSource {
    /// How what it claimed compares with the verified metadata; null when
    /// it made no claims about this torrent.
    #[schema(required = true)]
    pub claim: Option<ClaimDifferences>,
    /// It found the torrent first.
    pub first: bool,
    /// When it first found the torrent: the torrent's discovery time for
    /// the first source; null for the others, whose first sighting isn't
    /// recorded.
    #[schema(required = true)]
    pub first_seen_at: Option<i64>,
    /// The source's id, as `GET /sources` lists it.
    pub id: u8,
    /// The plugin's display name; its manifest id when this build doesn't
    /// run that plugin.
    pub name: String,
    /// The manifest id, as `source:` takes it.
    pub plugin: String,
    /// A private source, or its claims came through one (§18.7). Only the
    /// admin session ever sees one.
    pub private: bool,
}

/// What a source claimed that the verified metadata contradicts
/// (§18.5). A field is null when the source didn't state it or it
/// agrees; claimed file lists themselves are never returned. Names are
/// compared byte for byte, so a feed's tidied title differs: show it as
/// information, not a warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaimDifferences {
    /// Files claimed against files in the info dictionary (padding
    /// excluded).
    #[schema(required = true)]
    pub file_count: Option<ClaimedNumber>,
    /// Every stated field agrees.
    pub matches: bool,
    /// Both names are cut to 1,024 bytes.
    #[schema(required = true)]
    pub name: Option<ClaimedText>,
    /// Bytes.
    #[schema(required = true)]
    pub size: Option<ClaimedNumber>,
}

/// A claimed number and the verified one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaimedNumber {
    pub claimed: u64,
    pub verified: u64,
}

/// A claimed text and the verified one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaimedText {
    pub claimed: String,
    pub verified: String,
}

/// One tracker's report (`docs/metrics.md` §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SwarmTracker {
    /// When it answered.
    pub at: i64,
    pub completed: u32,
    pub leechers: u32,
    pub seeders: u32,
    pub url: String,
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
