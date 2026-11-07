//! Lodestar's companion manager core (architecture §13 `crates/companions/`,
//! §18.10; plan `docs/plan/2026-11-06-v06-companions.md`).
//!
//! - [`adapter`]: the detection, health and configuration traits each
//!   companion's adapter implements.
//! - [`catalogue`]: the compiled-in companion catalogue, its strict format
//!   and validation, and the trust root (owner §2.46, cq1 (a)).
//! - [`inputs`]: the closed, validated values generated compose services
//!   and Unraid templates are built from, and the shared [`CompanionId`]
//!   and [`Version`].
//!
//! Later CMP slices add their modules here, one `mod` line each.

pub mod adapter;
pub mod catalogue;
pub mod inputs;
pub mod log_events;

pub use catalogue::{Catalogue, CatalogueError, Entry, NotAvailable, builtin};
pub use inputs::{CompanionId, Version};
