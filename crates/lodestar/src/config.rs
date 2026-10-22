//! `config.toml` plus `LODESTAR_*` environment overrides (architecture
//! §11). Every field has a default, so an empty file (or none) works.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lodestar_dht::node::{self as dht, HarvestConfig, Intensity};
use lodestar_filter::Origin;
use lodestar_peer_wire::{Encryption, FetchOptions};
use lodestar_pipeline::PipelineConfig;
use lodestar_pipeline::health::HealthConfig;
use lodestar_search::RankWeights;
use lodestar_sources::PoolConfig;
use lodestar_store::StoreConfig;
use lodestar_tracker_scrape::TrackerUrl;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Holds `lodestar.db`, `dht-state.bin` and (by default) the config.
    pub data_dir: PathBuf,
    /// `error`, `warn`, `info`, `debug` or a `tracing` filter.
    pub log: String,
    pub http: Http,
    pub dht: Dht,
    pub fetch: Fetch,
    pub store: Store,
    pub search: Search,
    pub scrape: Scrape,
    pub ranking: Ranking,
    pub blocklist: Blocklist,
    pub subscriptions: Subscriptions,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            data_dir: PathBuf::from("data"),
            log: "info".into(),
            http: Http::default(),
            dht: Dht::default(),
            fetch: Fetch::default(),
            store: Store::default(),
            search: Search::default(),
            scrape: Scrape::default(),
            ranking: Ranking::default(),
            blocklist: Blocklist::default(),
            subscriptions: Subscriptions::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Http {
    pub bind: SocketAddr,
    /// Mark the session cookie `Secure` (set when serving over HTTPS,
    /// e.g. behind a TLS reverse proxy).
    pub secure_cookies: bool,
}

impl Default for Http {
    fn default() -> Self {
        Http {
            bind: SocketAddr::from(([0, 0, 0, 0], 8090)),
            secure_cookies: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntensityName {
    Low,
    Balanced,
    Aggressive,
}

impl From<IntensityName> for Intensity {
    fn from(i: IntensityName) -> Intensity {
        match i {
            IntensityName::Low => Intensity::Low,
            IntensityName::Balanced => Intensity::Balanced,
            IntensityName::Aggressive => Intensity::Aggressive,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dht {
    pub enabled: bool,
    /// At most one IPv4 and one IPv6 address.
    pub bind: Vec<SocketAddr>,
    pub intensity: IntensityName,
    pub virtual_ids: usize,
    pub bootstrap: Vec<String>,
    /// BEP 51 traversal.
    pub sample: bool,
    /// Harvest `get_peers`/`announce_peer` queries we receive.
    pub passive: bool,
    /// BEP 43: don't answer queries.
    pub read_only: bool,
}

impl Default for Dht {
    fn default() -> Self {
        let d = dht::Config::default();
        Dht {
            enabled: true,
            bind: d.bind,
            intensity: IntensityName::Balanced,
            virtual_ids: d.virtual_ids,
            bootstrap: d.bootstrap,
            sample: d.harvest.sample,
            passive: d.harvest.passive,
            read_only: d.read_only,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncryptionName {
    Prefer,
    Require,
    Disable,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Fetch {
    pub encryption: EncryptionName,
    /// Concurrent metadata fetches; unset follows the DHT intensity.
    pub max_sockets: Option<usize>,
    pub timeout_secs: u64,
    pub max_metadata_mib: usize,
}

impl Default for Fetch {
    fn default() -> Self {
        Fetch {
            encryption: EncryptionName::Prefer,
            max_sockets: None,
            timeout_secs: 8,
            max_metadata_mib: 16,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Store {
    pub mmap_mib: u64,
    /// Keep the info dict minus `pieces` (§3; off by default).
    pub archive_raw: bool,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            mmap_mib: 256,
            archive_raw: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Search {
    /// Keep the torrent index up to date (§8).
    pub enabled: bool,
    /// Index writer heap.
    pub heap_mib: usize,
}

impl Default for Search {
    fn default() -> Self {
        Search {
            enabled: true,
            heap_mib: 256,
        }
    }
}

/// Public UDP trackers that answer scrapes and have stayed up for years.
/// Scraping announces nothing, so our address never enters a swarm (§6).
pub const DEFAULT_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://explodie.org:6969/announce",
    "udp://tracker.theoks.net:6969/announce",
    "udp://opentracker.io:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scrape {
    /// Keep seeders and trackers-reporting fresh (§15).
    pub enabled: bool,
    /// Announce URLs (udp, http or https) to scrape.
    pub trackers: Vec<String>,
    /// BEP 33 DHT scrapes for new, viewed and rediscovered torrents.
    pub dht: bool,
    /// Seconds between requests to one UDP tracker; unset follows the
    /// DHT intensity (Low 5, Balanced 2, Aggressive 1).
    pub interval_secs: Option<u64>,
    /// Seconds between requests to one HTTP tracker; unset is three
    /// times the UDP interval.
    pub http_interval_secs: Option<u64>,
}

impl Default for Scrape {
    fn default() -> Self {
        Scrape {
            enabled: true,
            trackers: DEFAULT_TRACKERS.iter().map(|s| s.to_string()).collect(),
            dht: true,
            interval_secs: None,
            http_interval_secs: None,
        }
    }
}

/// Relevance priors (§8): each multiplies the text score, so text stays
/// first and these order near-ties. `?explain=1` shows their effect.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ranking {
    /// `1 + seeders · ln(1 + s)`.
    pub seeders: f32,
    /// Seeders assumed before a torrent's first scrape.
    pub unscraped_seeders: u32,
    /// Freshness never drops a score below this share.
    pub freshness_floor: f32,
    pub freshness_half_life_days: f32,
    /// `1 + trackers · min(t, cap) / cap`.
    pub trackers: f32,
    pub trackers_cap: u32,
    pub unscraped_trackers: u32,
    /// `1 − junk · score / 100`.
    pub junk: f32,
    /// The multiplier for a swarm flagged as fake seeding.
    pub suspect: f32,
}

impl Default for Ranking {
    fn default() -> Self {
        let w = RankWeights::default();
        Ranking {
            seeders: w.seeders,
            unscraped_seeders: w.unscraped_seeders,
            freshness_floor: w.freshness_floor,
            freshness_half_life_days: w.freshness_half_life_days,
            trackers: w.trackers,
            trackers_cap: w.trackers_cap,
            unscraped_trackers: w.unscraped_trackers,
            junk: w.junk,
            suspect: 0.2,
        }
    }
}

impl Ranking {
    pub fn weights(&self) -> RankWeights {
        RankWeights {
            seeders: self.seeders.max(0.0),
            unscraped_seeders: self.unscraped_seeders,
            freshness_floor: self.freshness_floor.clamp(0.0, 1.0),
            freshness_half_life_days: self.freshness_half_life_days.max(0.1),
            trackers: self.trackers.max(0.0),
            trackers_cap: self.trackers_cap.max(1),
            unscraped_trackers: self.unscraped_trackers,
            junk: self.junk.clamp(0.0, 1.0),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Blocklist {
    /// Files of hex info hashes, one per line (`#` starts a comment).
    /// Their hashes join the child-safety floor as imported CS-1b entries
    /// (§20.5.2); a hash removed from a file is no longer blocked.
    pub files: Vec<PathBuf>,
}

/// Blocklist subscriptions (§20.13). None by default: fetching a list
/// reveals your address to its host.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Subscriptions {
    /// Lists from the built-in catalog, by key (see `lodestar lists`).
    pub catalog: Vec<String>,
    /// Lists by URL.
    pub lists: Vec<SubscribedList>,
    /// Default refresh interval.
    pub refresh_hours: u64,
    /// Download cap (compressed).
    pub max_download_mib: u64,
    /// Cap on the expanded list.
    pub max_expanded_mib: u64,
}

impl Default for Subscriptions {
    fn default() -> Self {
        Subscriptions {
            catalog: Vec::new(),
            lists: Vec::new(),
            refresh_hours: 24,
            max_download_mib: 64,
            max_expanded_mib: 256,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribedList {
    pub url: String,
    pub name: Option<String>,
    pub kind: ListKind,
    /// Detected from the content when left out.
    pub format: Option<String>,
    /// For content lists: `drop` (default) or `quarantine`.
    #[serde(default)]
    pub action: ListAction,
    /// A vetted child-abuse hash list: its hashes join the locked floor as
    /// CS-1b (§20.13). Only for lists you have deliberately chosen.
    #[serde(default)]
    pub floor: bool,
    pub refresh_hours: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListKind {
    Content,
    Ip,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListAction {
    #[default]
    Drop,
    Quarantine,
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Reads `path` if given (it must exist), else `$LODESTAR_DATA_DIR/
    /// config.toml` or `data/config.toml` if present, then applies the
    /// environment.
    pub fn load(
        path: Option<&Path>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Config, ConfigError> {
        let data_dir = env("LODESTAR_DATA_DIR").map(PathBuf::from);
        let file = match path {
            Some(p) => Some(p.to_path_buf()),
            None => {
                let dir = data_dir.clone().unwrap_or_else(|| PathBuf::from("data"));
                let p = dir.join("config.toml");
                p.exists().then_some(p)
            }
        };
        let mut config = match file {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| ConfigError(format!("reading {}: {e}", p.display())))?;
                toml::from_str(&text).map_err(|e| ConfigError(format!("{}: {e}", p.display())))?
            }
            None => Config::default(),
        };
        if let Some(dir) = data_dir {
            config.data_dir = dir;
        }
        let parse = |name: &str, v: &str| ConfigError(format!("{name}: can't parse {v:?}"));
        if let Some(v) = env("LODESTAR_LOG") {
            config.log = v;
        }
        if let Some(v) = env("LODESTAR_HTTP_BIND") {
            config.http.bind = v.parse().map_err(|_| parse("LODESTAR_HTTP_BIND", &v))?;
        }
        if let Some(v) = env("LODESTAR_DHT_PORT") {
            let port: u16 = v.parse().map_err(|_| parse("LODESTAR_DHT_PORT", &v))?;
            for addr in &mut config.dht.bind {
                addr.set_port(port);
            }
        }
        if let Some(v) = env("LODESTAR_INTENSITY") {
            config.dht.intensity = toml::Value::String(v.clone())
                .try_into()
                .map_err(|_| parse("LODESTAR_INTENSITY", &v))?;
        }
        if let Some(v) = env("LODESTAR_DHT_ENABLED") {
            config.dht.enabled = v.parse().map_err(|_| parse("LODESTAR_DHT_ENABLED", &v))?;
        }
        if let Some(v) = env("LODESTAR_SCRAPE_ENABLED") {
            config.scrape.enabled = v
                .parse()
                .map_err(|_| parse("LODESTAR_SCRAPE_ENABLED", &v))?;
        }
        Ok(config)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("lodestar.db")
    }

    pub fn index_path(&self) -> PathBuf {
        lodestar_search::index_dir(&self.data_dir.join("index"))
    }

    pub fn store_config(&self) -> StoreConfig {
        StoreConfig {
            mmap_size: self.store.mmap_mib << 20,
            archive_raw: self.store.archive_raw,
            ..StoreConfig::default()
        }
    }

    pub fn dht_config(&self) -> dht::Config {
        dht::Config {
            bind: self.dht.bind.clone(),
            virtual_ids: self.dht.virtual_ids,
            intensity: self.dht.intensity.into(),
            bootstrap: self.dht.bootstrap.clone(),
            state_path: Some(self.data_dir.join("dht-state.bin")),
            read_only: self.dht.read_only,
            harvest: HarvestConfig {
                sample: self.dht.sample,
                passive: self.dht.passive,
            },
            ..dht::Config::default()
        }
    }

    pub fn pipeline_config(&self) -> Result<PipelineConfig, ConfigError> {
        // §6: 400 sockets at Balanced; the presets scale with it.
        let sockets = self.fetch.max_sockets.unwrap_or(match self.dht.intensity {
            IntensityName::Low => 100,
            IntensityName::Balanced => 400,
            IntensityName::Aggressive => 1000,
        });
        let fetch = FetchOptions {
            timeout: Duration::from_secs(self.fetch.timeout_secs.max(1)),
            max_metadata: self.fetch.max_metadata_mib << 20,
            encryption: match self.fetch.encryption {
                EncryptionName::Prefer => Encryption::Prefer,
                EncryptionName::Require => Encryption::Require,
                EncryptionName::Disable => Encryption::Disable,
            },
            ..FetchOptions::default()
        };
        let mut blocklist = HashSet::new();
        for path in &self.blocklist.files {
            let text = std::fs::read_to_string(path)
                .map_err(|e| ConfigError(format!("blocklist {}: {e}", path.display())))?;
            parse_blocklist(&text, &mut blocklist).map_err(|line| {
                ConfigError(format!("blocklist {}: bad line {line}", path.display()))
            })?;
        }
        let filter = lodestar_pipeline::default_filter();
        filter.update(|core| {
            for hash in blocklist {
                core.cs.add_hash(hash, Origin::Imported);
            }
        });
        Ok(PipelineConfig {
            pool: PoolConfig {
                max_sockets: sockets,
                ..PoolConfig::default()
            },
            fetch,
            filter: Arc::new(filter),
            ..PipelineConfig::default()
        })
    }
}

impl Config {
    /// The scrape scheduler's settings and trackers, or `None` when
    /// scraping is off. Tracker URLs that name the same tracker count
    /// once; a URL that can't be scraped is an error, so typos surface.
    pub fn health_config(&self) -> Result<Option<(HealthConfig, Vec<TrackerUrl>)>, ConfigError> {
        if !self.scrape.enabled {
            return Ok(None);
        }
        let mut trackers: Vec<TrackerUrl> = Vec::new();
        // Errors name the entry by position: an HTTP tracker URL may carry
        // a passkey, and errors end up in logs.
        for (i, raw) in self.scrape.trackers.iter().enumerate() {
            let entry = i + 1;
            let url = TrackerUrl::parse(raw)
                .map_err(|e| ConfigError(format!("scrape.trackers entry {entry}: {e}")))?;
            if url.scheme() != lodestar_tracker_scrape::Scheme::Udp && url.scrape_url().is_none() {
                return Err(ConfigError(format!(
                    "scrape.trackers entry {entry}: an HTTP tracker URL must end in /announce \
                     (BEP 48) to be scraped"
                )));
            }
            if !trackers.iter().any(|t| t.as_str() == url.as_str()) {
                trackers.push(url);
            }
        }
        let interval = Duration::from_secs(self.scrape.interval_secs.unwrap_or(
            match self.dht.intensity {
                IntensityName::Low => 5,
                IntensityName::Balanced => 2,
                IntensityName::Aggressive => 1,
            },
        ))
        .max(Duration::from_secs(1));
        let http_interval = self
            .scrape
            .http_interval_secs
            .map_or(interval * 3, Duration::from_secs)
            .max(Duration::from_secs(1));
        // Scrapes leave from the crawler's address, so they take the same
        // route (and VPN) as the DHT (§5).
        let bind = self
            .dht
            .bind
            .iter()
            .find(|a| a.is_ipv4() && !a.ip().is_unspecified())
            .map(|a| SocketAddr::new(a.ip(), 0));
        let mut cfg = HealthConfig {
            interval,
            http_interval,
            dht: self.scrape.dht && self.dht.enabled,
            ..HealthConfig::default()
        };
        cfg.udp.bind = bind;
        Ok(Some((cfg, trackers)))
    }
}

/// Hex info hashes, one per line; blank lines and `#` comments skipped.
/// Returns the 1-based number of the first bad line.
pub fn parse_blocklist(text: &str, out: &mut HashSet<[u8; 20]>) -> Result<(), usize> {
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let hash = parse_hex20(line).ok_or(n + 1)?;
        out.insert(hash);
    }
    Ok(())
}

fn parse_hex20(s: &str) -> Option<[u8; 20]> {
    let bytes = s.as_bytes();
    if bytes.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, pair) in bytes.chunks(2).enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_follow_the_plan() {
        let c = Config::load(None, no_env).unwrap();
        assert_eq!(c.http.bind.port(), 8090);
        assert_eq!(c.dht.bind[0].port(), 6881);
        assert!(c.dht.enabled);
        let p = c.pipeline_config().unwrap();
        assert_eq!(p.pool.max_sockets, 400);
        assert_eq!(p.fetch.encryption, Encryption::Prefer);
        assert_eq!(
            c.dht_config().state_path,
            Some(PathBuf::from("data").join("dht-state.bin"))
        );
    }

    #[test]
    fn file_then_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
            data_dir = "/srv/lodestar"
            [dht]
            intensity = "low"
            bind = ["0.0.0.0:7000", "[::]:7000"]
            [fetch]
            encryption = "require"
            "#,
        )
        .unwrap();
        let env = |k: &str| match k {
            "LODESTAR_DHT_PORT" => Some("6999".to_string()),
            "LODESTAR_HTTP_BIND" => Some("127.0.0.1:9000".to_string()),
            "LODESTAR_INTENSITY" => Some("aggressive".to_string()),
            _ => None,
        };
        let c = Config::load(Some(&path), env).unwrap();
        assert_eq!(c.data_dir, PathBuf::from("/srv/lodestar"));
        assert!(c.dht.bind.iter().all(|a| a.port() == 6999));
        assert_eq!(c.http.bind, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(c.dht.intensity, IntensityName::Aggressive);
        assert_eq!(c.pipeline_config().unwrap().pool.max_sockets, 1000);
        assert_eq!(
            c.pipeline_config().unwrap().fetch.encryption,
            Encryption::Require
        );
    }

    #[test]
    fn scrape_settings() {
        let c = Config::load(None, no_env).unwrap();
        let (h, trackers) = c.health_config().unwrap().unwrap();
        assert_eq!(trackers.len(), DEFAULT_TRACKERS.len());
        assert_eq!(h.interval, Duration::from_secs(2));
        assert_eq!(h.http_interval, Duration::from_secs(6));
        assert!(h.dht);
        assert_eq!(h.udp.bind, None, "unspecified DHT bind: any address");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
            [dht]
            intensity = "low"
            bind = ["10.8.0.2:6881"]
            [scrape]
            trackers = ["udp://a.example:1/announce", "udp://A.example:1", "http://b.example/announce"]
            "#,
        )
        .unwrap();
        let c = Config::load(Some(&path), no_env).unwrap();
        let (h, trackers) = c.health_config().unwrap().unwrap();
        assert_eq!(trackers.len(), 2, "the same tracker counts once");
        assert_eq!(h.interval, Duration::from_secs(5));
        assert_eq!(h.udp.bind, Some("10.8.0.2:0".parse().unwrap()));

        std::fs::write(&path, "[scrape]\ntrackers = [\"wss://x.example\"]\n").unwrap();
        let c = Config::load(Some(&path), no_env).unwrap();
        assert!(c.health_config().is_err());
        std::fs::write(
            &path,
            "[scrape]\ntrackers = [\"udp://a.example:1\", \"https://t.example/secret123/x\"]\n",
        )
        .unwrap();
        let c = Config::load(Some(&path), no_env).unwrap();
        let e = c.health_config().unwrap_err().to_string();
        assert!(e.contains("entry 2") && !e.contains("secret123"), "{e}");

        let off = |k: &str| (k == "LODESTAR_SCRAPE_ENABLED").then(|| "false".to_string());
        let c = Config::load(None, off).unwrap();
        assert!(c.health_config().unwrap().is_none());
    }

    #[test]
    fn ranking_weights() {
        let c = Config::load(None, no_env).unwrap();
        assert_eq!(c.ranking.weights(), RankWeights::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[ranking]\nseeders = 0.5\nfreshness_floor = 3.0\n").unwrap();
        let w = Config::load(Some(&path), no_env).unwrap().ranking.weights();
        assert_eq!(w.seeders, 0.5);
        assert_eq!(w.freshness_floor, 1.0, "clamped");
        std::fs::write(&path, "[ranking]\nseeder = 0.5\n").unwrap();
        assert!(Config::load(Some(&path), no_env).is_err());
    }

    #[test]
    fn typos_and_bad_values_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[dht]\nintensty = \"low\"\n").unwrap();
        assert!(Config::load(Some(&path), no_env).is_err());
        let bad = |k: &str| (k == "LODESTAR_INTENSITY").then(|| "turbo".to_string());
        assert!(Config::load(None, bad).is_err());
        assert!(Config::load(Some(&dir.path().join("missing.toml")), no_env).is_err());
    }

    #[test]
    fn blocklists_parse_hex_lines() {
        let mut set = HashSet::new();
        let text = "# comment\n\nABCDEF0123456789abcdef0123456789abcdef01  # trailing\n";
        parse_blocklist(text, &mut set).unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.contains(&parse_hex20("abcdef0123456789abcdef0123456789abcdef01").unwrap()));
        assert_eq!(parse_blocklist("zz\n", &mut set), Err(1));
        assert_eq!(parse_blocklist("\n\nabcd", &mut set), Err(3));
    }
}
