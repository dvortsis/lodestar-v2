//! Mainline DHT (architecture §5).
//!
//! M1 builds this in slices: the KRPC wire codec ([`krpc`], [`compact`])
//! comes first, then the routing table, then the harvesting node.

pub mod compact;
pub mod krpc;

pub use compact::{NodeId, NodeInfo};
