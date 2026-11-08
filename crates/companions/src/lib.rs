//! Lodestar's companion manager core (architecture §13 `crates/companions/`,
//! §18.10; plan `docs/plan/2026-11-06-v06-companions.md`).
//!
//! - [`adapter`]: the detection, health and configuration traits each
//!   companion's adapter implements.
//! - [`catalogue`]: the compiled-in companion catalogue, its strict format
//!   and validation, and the trust root (owner §2.46, cq1 (a)).
//! - [`credentials`]: companion credentials, generated, sealed in
//!   `settings` rows, resealed on key rotation (CMP-3).
//! - [`config_files`]: `qBittorrent.conf` and Prowlarr's `config.xml`,
//!   written before first start with the credential and a loopback bind
//!   (CMP-3).
//! - [`detect`]: finding qBittorrent, Prowlarr and gluetun on fixed
//!   targets or a URL you enter, and their health, through the SSRF guard
//!   (CMP-8a).
//! - [`fetch`] and [`verify`]: a pinned artifact's download through the
//!   entry's host allowlist, and its size, SHA-256 and upstream signature
//!   checks (CMP-1).
//! - [`inputs`]: the closed, validated values generated compose services
//!   and Unraid templates are built from, and the shared [`CompanionId`]
//!   and [`Version`].
//! - [`supervise`]: starting, restarting, health-checking and stopping
//!   managed companions, their redacted log rings, and orphan cleanup
//!   (CMP-4a).
//!
//! Later CMP slices add their modules here, one `mod` line each.

pub mod adapter;
pub mod catalogue;
pub mod config_files;
pub mod credentials;
pub mod detect;
pub mod extract;
pub mod fetch;
pub mod inputs;
pub mod layout;
pub mod log_events;
pub mod supervise;
pub mod verify;

pub use catalogue::{Catalogue, CatalogueError, Entry, NotAvailable, builtin};
pub use inputs::{CompanionId, Version};
