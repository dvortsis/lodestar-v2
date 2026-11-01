//! Sources: the plugin API and the crawl scheduler (architecture §18).
//!
//! Every way Lodestar learns about torrents is a source plugin (§18.1):
//! the Mainline DHT is plugin #1, and importers, feeds and overlay
//! networks follow on the same contract. Keys from every source meet in
//! one shared metadata-fetch pool (§18.4), scheduled fairly per source
//! under global hard caps.

pub mod claims;
mod funnel;
pub mod http;
mod manifest;
mod plugin;
pub mod pool;
pub mod state;
pub mod testing;

pub use claims::{ClaimedFile, Claims, Confidence, ExternalId, Provenance};
pub use funnel::{Funnel, FunnelCounts, Loss, Stage};
pub use http::{Client, Fetch, HostLimiter, HostState, HttpError, Request, Response};
pub use manifest::{
    Backoff, Caps, Consent, Distribution, EMPTY_SCHEMA, Endpoint, HostRate, Kind, Limits, Manifest,
    Network, Policy, Priority, Risk, Schedule, UserLimits, Window,
};
pub use plugin::{
    BoxFuture, Closed, Discovery, Emitter, Found, Kit, LookupSession, PeerRequest, PluginError,
    SessionAdmission, Shutdown, SourceCtx, SourcePlugin, Terminal,
};
pub use pool::{FetchedKey, Fetcher, PeerWireFetcher, PoolConfig, PoolHandle, PoolIo, PoolStats};
pub use state::{MemoryState, PluginState, StateError};
