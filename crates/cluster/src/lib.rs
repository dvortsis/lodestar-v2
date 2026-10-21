//! The coordinator–worker boundary (architecture §19).
//!
//! One coordinator owns the store, the index and the UI; workers find,
//! fetch and prepare torrents. M3 builds the boundary with one built-in
//! worker in-process ([`WorkerId::LOCAL`]), so all processing already runs
//! on the worker side and single-machine behaviour is unchanged. Remote
//! workers (v0.6) add the network protocol, joining and the replicated
//! have-filter on top of the same pieces.
//!
//! This crate holds the parts that don't depend on how the two sides talk:
//! the fleet-wide [`LeaseTable`] with its fencing tokens.

mod leases;

pub use leases::{Claim, Claimed, Lease, LeaseTable, RestoredLease, Token, WorkerId};
