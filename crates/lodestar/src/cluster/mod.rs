//! The coordinator's side of remote workers in `lodestar serve`
//! (architecture §19.2, §19.6; workers plan WK-1).
//!
//! - [`serve`]: the cluster port. It follows "Allow workers" (`[cluster]
//!   enabled`): while that's off, nothing is bound and no cluster CA is
//!   opened or created, so a single-machine install has no cluster port,
//!   no cluster `settings` rows and no cluster keys (§19.6 "created on
//!   first use", plan Q3). Turning it on binds the port, opens the PKI and
//!   builds the `ClusterServer` on the pipeline's lease authority.
//!
//! Later slices add one `mod` line each here (placement, intake, ops…).

pub mod filter;
pub mod have;
mod intake;
pub mod ops;
pub mod placement;
pub mod serve;
#[cfg(test)]
mod testing;

pub use serve::{Cluster, ClusterHandle, Wiring};
