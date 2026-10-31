//! Sources: the plugin API and the crawl scheduler (architecture §18).
//!
//! Every way Lodestar learns about torrents is a source plugin (§18.1):
//! the Mainline DHT is plugin #1, and importers, feeds and overlay
//! networks follow on the same contract. Keys from every source meet in
//! one shared metadata-fetch pool (§18.4), scheduled fairly per source
//! under global hard caps.

mod funnel;
mod manifest;
mod plugin;
pub mod pool;

pub use funnel::{Funnel, FunnelCounts, Loss, Stage};
pub use manifest::{Consent, Endpoint, Kind, Manifest, Network, Priority, Risk};
pub use plugin::{
    BoxFuture, Closed, Discovery, Emitter, Found, LookupSession, PeerRequest, PluginError,
    SessionAdmission, Shutdown, SourceCtx, SourcePlugin, Terminal,
};
pub use pool::{FetchedKey, Fetcher, PeerWireFetcher, PoolConfig, PoolHandle, PoolIo, PoolStats};
