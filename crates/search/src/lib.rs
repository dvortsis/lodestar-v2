//! The search engine (architecture §8): the name tokenizer, the torrent
//! index schema, near-real-time indexing that follows the store, and
//! queries, paged through searcher leases and cursors.

mod cancel;
mod compile;
mod count;
mod cursor;
mod doc;
pub mod facets;
pub mod files;
mod filter;
mod handle;
mod index;
mod lease;
mod live;
pub mod query;
mod rebuild;
pub mod schema;
mod sorted;
mod stem;
#[cfg(test)]
mod testing;
pub mod tokenizer;

pub use cancel::{CancelOnDrop, CancelToken};
pub use count::{EXACT_UP_TO, Total};
pub use cursor::{Cursor, LeaseId, Position};
pub use doc::{DocEnrichment, TorrentDoc, extension, flag_names};
pub use facets::{
    Bin, Facet, FacetParams, Facets, Histo, Histogram, TermCounts, Unit, ValueCount, facets,
};
pub use handle::SearchHandle;
pub use index::{
    Compiled, Error, Hit, Indexer, Result, Results, STEM_FALLBACK_BELOW, SearchIndex, Sort,
    index_dir,
};
pub use lease::{EXACT_BUDGET, Lease, LeaseConfig, Leases, Page, Start};
pub use live::{IndexStats, LiveConfig, LiveIndexer};
pub use query::{Node, ParseError, parse};
pub use rebuild::{IndexVersion, Rebuild, RebuildPhase, RebuildProgress, Serving, ServingConfig};
