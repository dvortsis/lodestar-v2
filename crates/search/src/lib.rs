//! The search engine (architecture §8): the name tokenizer, the torrent
//! index schema, near-real-time indexing that follows the store, and
//! queries, paged through searcher leases and cursors.

mod cache;
mod cancel;
mod compile;
mod count;
mod cursor;
mod doc;
mod explain;
pub mod facets;
pub mod files;
mod filter;
mod handle;
mod index;
mod lease;
mod live;
pub mod log_events;
pub mod natural;
mod percolate;
pub mod query;
mod rebuild;
pub mod recovery;
mod retry_dir;
pub mod schema;
mod seek_guard;
mod sorted;
mod stem;
pub mod suggest;
#[cfg(test)]
mod testing;
pub mod tokenizer;

pub use cache::{
    CacheConfig, CacheStats, CachedFacets, CachedSearch, FacetSpec, Generation, KindStats, Outcome,
    ResultCache, SearchSpec,
};
pub use cancel::{CancelOnDrop, CancelToken};
pub use count::{EXACT_UP_TO, Total};
pub use cursor::{Cursor, LeaseId, Position};
pub use doc::{DocEnrichment, TorrentDoc, extension, flag_names};
pub use explain::{EXPLAIN_MAX, Explained, Explanation, Parts, Priors, WEIGHTS, Weights, explain};
pub use facets::{
    Bin, Facet, FacetParams, Facets, Histo, Histogram, TermCounts, Unit, ValueCount, facets,
    with_filters,
};
pub use filter::{in_time, minute_of_day};
pub use handle::SearchHandle;
pub use index::{
    CacheUsage, CatchUp, Compiled, Error, Hit, Indexer, Penalty, RankWeights, Result, Results,
    STEM_FALLBACK_BELOW, ScoreParts, SearchIndex, Sort, index_dir,
};
pub use lease::{EXACT_BUDGET, Lease, LeaseConfig, Leases, Page, Start};
pub use live::{CommitHook, Committed, IndexStats, LiveConfig, LiveIndexer};
pub use percolate::{Due, Matched, PercolateLimits, Percolation, percolate};
pub use query::{Node, ParseError, parse};
pub use rebuild::{
    IndexVersion, Ranking, Rebuild, RebuildPhase, RebuildProgress, Serving, ServingConfig,
};
