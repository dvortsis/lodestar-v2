//! Mainline DHT (architecture §5).
//!
//! M1 builds this in slices: the KRPC wire codec ([`krpc`], [`compact`])
//! comes first, then the routing table ([`routing`]) with BEP 42 secure ids
//! ([`secure_id`]) and warm-restart state ([`state`]), then the harvesting
//! node.

pub mod compact;
pub mod krpc;
pub mod node;
pub mod routing;
pub mod secure_id;
pub mod state;

pub use compact::{NodeId, NodeInfo};
