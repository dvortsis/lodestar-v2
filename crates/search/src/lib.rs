//! The search engine (architecture §8): the name tokenizer, the torrent
//! index schema, near-real-time indexing that follows the store, and
//! queries.

mod compile;
mod doc;
mod index;
mod live;
pub mod query;
pub mod schema;
pub mod tokenizer;

pub use doc::{TorrentDoc, extension, flag_names};
pub use index::{Error, Hit, Indexer, Result, Results, SearchIndex, index_dir};
pub use live::{IndexStats, LiveConfig, LiveIndexer};
pub use query::{Node, ParseError, parse};
