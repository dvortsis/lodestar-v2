//! The search engine (architecture §8): the name tokenizer, the torrent
//! index schema, near-real-time indexing that follows the store, and
//! queries.

mod compile;
mod doc;
pub mod files;
mod filter;
mod index;
mod live;
pub mod query;
mod rebuild;
pub mod schema;
mod sorted;
mod stem;
pub mod tokenizer;

pub use doc::{DocEnrichment, TorrentDoc, extension, flag_names};
pub use index::{
    Compiled, Error, Hit, Indexer, Result, Results, STEM_FALLBACK_BELOW, SearchIndex, Sort,
    index_dir,
};
pub use live::{IndexStats, LiveConfig, LiveIndexer};
pub use query::{Node, ParseError, parse};
pub use rebuild::{IndexVersion, Rebuild, RebuildPhase, RebuildProgress, Serving, ServingConfig};
