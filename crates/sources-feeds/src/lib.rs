//! Parsers for the feeds discovery sources read (architecture §18.2).
//!
//! Every parser here is a pure function from untrusted bytes to items: no
//! I/O, no unbounded allocation, no panics. Each caps as it reads (see
//! `lodestar_sources::claims`) and stops with a typed reason at a cap,
//! keeping only the items it finished.

pub mod anidb;
pub mod anilist;
pub mod archive_org;
mod common;
pub mod discogs;
pub mod igdb;
pub mod json;
pub mod lemmy;
pub mod musicbrainz;
pub mod nostr;
pub mod nostr_sig;
pub mod openlibrary;
pub mod predb;
pub mod rss;
pub mod srrdb;
pub mod stashdb;
pub mod steam;
pub mod tmdb;
pub mod torznab;
pub mod tpdb;
pub mod tvdb;
pub mod wikidata;

pub use common::external_ref;
