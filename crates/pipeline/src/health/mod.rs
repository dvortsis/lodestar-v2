//! Swarm health (architecture §6, §15, §16 M4 plan): the scrape
//! scheduler that keeps seeders, leechers and trackers-reporting fresh
//! within per-tracker limits.
//!
//! The pure parts live in submodules: when to scrape again
//! ([`schedule`]), which torrents a round takes ([`lanes`]), per-tracker
//! pacing and health ([`trackers`]) and how one round's answers become a
//! swarm record ([`aggregate`]).

pub mod aggregate;
mod client;
pub mod lanes;
pub mod schedule;
mod service;
pub mod trackers;

pub use client::{Outcome, TrackerStatus};
pub use service::{HealthConfig, HealthHandle, HealthStats, register_trackers};
pub(crate) use service::{Setup, spawn};
