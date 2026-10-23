//! `config.toml` plus `LODESTAR_*` environment overrides (architecture
//! §11). Every field has a default, so an empty file (or none) works.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lodestar_dht::node::{self as dht, HarvestConfig, Intensity};
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
    /// Which values `config.toml` and the environment set, for the
    /// settings registry's sources (`settings`). Not a config key.
    #[serde(skip)]
    pub layers: Layers,
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
    pub content: Content,
    pub blocklist: Blocklist,
    pub subscriptions: Subscriptions,
    pub updates: Updates,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            layers: Layers::default(),
            data_dir: PathBuf::from("data"),
            log: "info".into(),
            http: Http::default(),
            dht: Dht::default(),
            fetch: Fetch::default(),
            store: Store::default(),
            search: Search::default(),
            scrape: Scrape::default(),
            ranking: Ranking::default(),
            content: Content::default(),
            blocklist: Blocklist::default(),
            subscriptions: Subscriptions::default(),
            updates: Updates::default(),
        }
    }
}

/// What set a [`Config`]'s values beyond the defaults.
#[derive(Debug, Clone, Default)]
pub struct Layers {
    /// `config.toml` as written (empty without a file).
    pub file: toml::Table,
    /// The `LODESTAR_*` variables that were set, from [`ENV_VARS`].
    pub env: Vec<&'static str>,
}

impl Layers {
    /// Whether the file sets the key at a dotted `path` (`dht.bind`).
    pub fn in_file(&self, path: &str) -> bool {
        let mut table = &self.file;
        let mut parts = path.split('.').peekable();
        while let Some(part) = parts.next() {
            match table.get(part) {
                None => return false,
                Some(_) if parts.peek().is_none() => return true,
                Some(toml::Value::Table(t)) => table = t,
                Some(_) => return false,
            }
        }
        false
    }

    pub fn in_env(&self, var: &str) -> bool {
        self.env.contains(&var)
    }
}

/// Every environment variable [`Config::load`] reads.
pub const ENV_VARS: &[&str] = &[
    "LODESTAR_DATA_DIR",
    "LODESTAR_LOG",
    "LODESTAR_HTTP_BIND",
    "LODESTAR_DHT_PORT",
    "LODESTAR_INTENSITY",
    "LODESTAR_DHT_ENABLED",
    "LODESTAR_SECURE_COOKIES",
    "LODESTAR_SHOW_ADULT",
    "LODESTAR_UPDATES",
    "LODESTAR_UPDATES_LEVEL",
    "LODESTAR_UPDATES_SECURITY_FIXES",
    "LODESTAR_UPDATES_CHANNEL",
    "LODESTAR_UPDATES_WAIT_DAYS",
    "LODESTAR_UPDATES_WINDOW",
    "LODESTAR_UPDATES_ALLOW_REINDEX",
    "LODESTAR_UPDATES_NOTIFY_BEFORE",
    "LODESTAR_UPDATES_NOTIFY_AFTER",
    "LODESTAR_UPDATES_NOTIFY_FAILURE",
];

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
    /// Concurrent metadata fetches; unset or 0 follows the DHT intensity.
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
    /// Refuses a weight that isn't a finite number: TOML takes `nan` and
    /// `inf`, and clamping passes NaN through, which would make every
    /// relevance score NaN.
    fn check(&self) -> Result<(), ConfigError> {
        let fields = [
            ("seeders", self.seeders),
            ("freshness_floor", self.freshness_floor),
            ("freshness_half_life_days", self.freshness_half_life_days),
            ("trackers", self.trackers),
            ("junk", self.junk),
            ("suspect", self.suspect),
        ];
        match fields.iter().find(|(_, v)| !v.is_finite()) {
            Some((name, _)) => Err(ConfigError(format!(
                "ranking.{name} must be a finite number"
            ))),
            None => Ok(()),
        }
    }

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

/// What's shown (§20.9). Set here or in `LODESTAR_SHOW_ADULT`, Adult is
/// shown without the app's 18+ confirmation: whoever edits the server's
/// configuration has decided.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Content {
    /// Show adult content in search (off by default).
    pub show_adult: bool,
}

/// Self-updates (§12.2, owner §2.13): off until chosen. The settings
/// section `updates` shows and saves the same keys.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Updates {
    pub mode: UpdateMode,
    pub level: UpdateLevel,
    pub security_fixes: SecurityFixes,
    pub channel: UpdateChannel,
    /// Days a release waits after its date before it installs (0–14).
    pub wait_days: u32,
    /// The install window, local time, `HH:MM-HH:MM` (may wrap midnight).
    pub window: String,
    /// Install releases that rebuild the search index.
    pub allow_reindex: bool,
    pub notify_before: bool,
    pub notify_after: bool,
    pub notify_failure: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Updates {
            mode: UpdateMode::Off,
            level: UpdateLevel::Fixes,
            security_fixes: SecurityFixes::Auto,
            channel: UpdateChannel::Stable,
            wait_days: 3,
            window: "03:00-05:00".into(),
            allow_reindex: true,
            notify_before: false,
            notify_after: true,
            notify_failure: true,
        }
    }
}

impl Updates {
    /// The checks a value from the file or the environment must pass
    /// (the settings section repeats them for the app's own).
    pub fn check(&self) -> Result<(), ConfigError> {
        if self.wait_days > 14 {
            return Err(ConfigError(format!(
                "updates.wait_days: {} is more than 14",
                self.wait_days
            )));
        }
        crate::update::window::Window::parse(&self.window)
            .map_err(|e| ConfigError(format!("updates.window: {e}")))?;
        Ok(())
    }
}

/// `off` (the default), `notify` (check and notify), `auto` (install
/// automatically) or `image` (the container image updates it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    #[default]
    Off,
    Notify,
    Auto,
    Image,
}

/// Which releases install automatically: fixes ≤ features ≤ all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateLevel {
    #[default]
    Fixes,
    Features,
    All,
}

/// "Always install security fixes": `auto` is on under `auto`, off
/// under `notify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityFixes {
    #[default]
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
    Nightly,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Blocklist {
    /// Files of hex info hashes, one per line (`#` starts a comment).
    /// Their hashes join the child-safety floor as imported CS-1b entries
    /// (§20.5.2). A file only adds (owner's decision §2.7): a hash deleted
    /// from it, or a file dropped from this list, stays blocked until it's
    /// removed in the app with the effect warning and the password.
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
        let mut file_table = toml::Table::new();
        let mut config: Config = match file {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| ConfigError(format!("reading {}: {e}", p.display())))?;
                file_table = toml::from_str(&text)
                    .map_err(|e| ConfigError(format!("{}: {e}", p.display())))?;
                toml::from_str(&text).map_err(|e| ConfigError(format!("{}: {e}", p.display())))?
            }
            None => Config::default(),
        };
        config.layers = Layers {
            file: file_table,
            env: ENV_VARS
                .iter()
                .copied()
                .filter(|k| env(k).is_some())
                .collect(),
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
        if let Some(v) = env("LODESTAR_SECURE_COOKIES") {
            config.http.secure_cookies = v
                .parse()
                .map_err(|_| parse("LODESTAR_SECURE_COOKIES", &v))?;
        }
        if let Some(v) = env("LODESTAR_SHOW_ADULT") {
            config.content.show_adult = v.parse().map_err(|_| parse("LODESTAR_SHOW_ADULT", &v))?;
        }
        config.updates_from_env(&env)?;
        config.ranking.check()?;
        config.updates.check()?;
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
        // §6: 400 sockets at Balanced; the presets scale with it. 0, as
        // unset, follows the intensity (as the settings registry shows it).
        let sockets =
            self.fetch
                .max_sockets
                .filter(|&n| n > 0)
                .unwrap_or(match self.dht.intensity {
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
        // Config files add to the floor through the store at start
        // (`floor::sync_files`), so the store's floor is the only one.
        let filter = lodestar_pipeline::default_filter();
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
    /// The `LODESTAR_UPDATES*` variables.
    fn updates_from_env(
        &mut self,
        env: &impl Fn(&str) -> Option<String>,
    ) -> Result<(), ConfigError> {
        fn word<T: serde::de::DeserializeOwned>(name: &str, v: &str) -> Result<T, ConfigError> {
            toml::Value::String(v.to_string())
                .try_into()
                .map_err(|_| ConfigError(format!("{name}: can't parse {v:?}")))
        }
        fn flag(name: &str, v: &str) -> Result<bool, ConfigError> {
            v.parse()
                .map_err(|_| ConfigError(format!("{name}: can't parse {v:?}")))
        }
        let u = &mut self.updates;
        if let Some(v) = env("LODESTAR_UPDATES") {
            u.mode = word("LODESTAR_UPDATES", &v)?;
        }
        if let Some(v) = env("LODESTAR_UPDATES_LEVEL") {
            u.level = word("LODESTAR_UPDATES_LEVEL", &v)?;
        }
        if let Some(v) = env("LODESTAR_UPDATES_SECURITY_FIXES") {
            u.security_fixes = word("LODESTAR_UPDATES_SECURITY_FIXES", &v)?;
        }
        if let Some(v) = env("LODESTAR_UPDATES_CHANNEL") {
            u.channel = word("LODESTAR_UPDATES_CHANNEL", &v)?;
        }
        if let Some(v) = env("LODESTAR_UPDATES_WAIT_DAYS") {
            u.wait_days = v.parse().map_err(|_| {
                ConfigError(format!("LODESTAR_UPDATES_WAIT_DAYS: can't parse {v:?}"))
            })?;
        }
        if let Some(v) = env("LODESTAR_UPDATES_WINDOW") {
            u.window = v;
        }
        if let Some(v) = env("LODESTAR_UPDATES_ALLOW_REINDEX") {
            u.allow_reindex = flag("LODESTAR_UPDATES_ALLOW_REINDEX", &v)?;
        }
        if let Some(v) = env("LODESTAR_UPDATES_NOTIFY_BEFORE") {
            u.notify_before = flag("LODESTAR_UPDATES_NOTIFY_BEFORE", &v)?;
        }
        if let Some(v) = env("LODESTAR_UPDATES_NOTIFY_AFTER") {
            u.notify_after = flag("LODESTAR_UPDATES_NOTIFY_AFTER", &v)?;
        }
        if let Some(v) = env("LODESTAR_UPDATES_NOTIFY_FAILURE") {
            u.notify_failure = flag("LODESTAR_UPDATES_NOTIFY_FAILURE", &v)?;
        }
        Ok(())
    }
}

/// A `blocklist.files` path, as written, and its hashes.
pub type BlocklistFile = (String, Vec<[u8; 20]>);

impl Config {
    /// Each `blocklist.files` entry's hashes, keyed by the path as written.
    pub fn blocklist_files(&self) -> Result<Vec<BlocklistFile>, ConfigError> {
        let mut out = Vec::new();
        for path in &self.blocklist.files {
            let text = std::fs::read_to_string(path)
                .map_err(|e| ConfigError(format!("blocklist {}: {e}", path.display())))?;
            let mut hashes = HashSet::new();
            parse_blocklist(&text, &mut hashes).map_err(|line| {
                ConfigError(format!("blocklist {}: bad line {line}", path.display()))
            })?;
            out.push((path.display().to_string(), hashes.into_iter().collect()));
        }
        Ok(out)
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
    fn layers_record_what_the_file_and_environment_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "log = \"debug\"\n[dht]\nintensity = \"low\"\n").unwrap();
        let env = |k: &str| (k == "LODESTAR_HTTP_BIND").then(|| "127.0.0.1:1".to_string());
        let c = Config::load(Some(&path), env).unwrap();
        assert!(c.layers.in_file("log"));
        assert!(c.layers.in_file("dht.intensity"));
        assert!(!c.layers.in_file("dht.bind"));
        assert!(!c.layers.in_file("log.level"));
        assert!(!c.layers.in_file("http.bind"));
        assert_eq!(c.layers.env, ["LODESTAR_HTTP_BIND"]);
        assert!(c.layers.in_env("LODESTAR_HTTP_BIND"));
        let c = Config::load(None, no_env).unwrap();
        assert!(c.layers.file.is_empty() && c.layers.env.is_empty());
    }

    #[test]
    fn zero_sockets_follow_the_intensity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[fetch]\nmax_sockets = 0\n[dht]\nintensity = \"low\"\n",
        )
        .unwrap();
        let c = Config::load(Some(&path), no_env).unwrap();
        assert_eq!(c.pipeline_config().unwrap().pool.max_sockets, 100);
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
        for bad in ["seeders = nan", "junk = inf", "suspect = -inf"] {
            std::fs::write(&path, format!("[ranking]\n{bad}\n")).unwrap();
            let e = Config::load(Some(&path), no_env).unwrap_err();
            let field = bad.split(' ').next().unwrap();
            assert!(e.0.contains(&format!("ranking.{field}")), "{bad}: {e}");
        }
    }
}
