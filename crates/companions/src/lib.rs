//! Lodestar's companion manager core (architecture §13 `crates/companions/`,
//! §18.10; plan `docs/plan/2026-11-06-v06-companions.md`).
//!
//! - [`adapter`]: the detection, health and configuration traits each
//!   companion's adapter implements.
//! - [`inputs`]: the closed, validated values generated compose services
//!   and Unraid templates are built from, and the shared [`CompanionId`]
//!   and [`Version`].
//!
//! The compiled-in catalogue (CMP-0) and later CMP slices add their
//! modules here, one `mod` line each.

pub mod adapter;
pub mod inputs;
pub mod log_events;

pub use inputs::{CompanionId, Version};
