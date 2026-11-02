//! Parsers for the feeds discovery sources read (architecture §18.2).
//!
//! Every parser here is a pure function from untrusted bytes to items: no
//! I/O, no unbounded allocation, no panics. Each caps as it reads (see
//! `lodestar_sources::claims`) and stops with a typed reason at a cap,
//! keeping only the items it finished.

mod common;
pub mod rss;
pub mod torznab;

pub use common::external_ref;
