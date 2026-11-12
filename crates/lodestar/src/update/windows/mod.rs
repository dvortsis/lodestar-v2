//! Self-updates on Windows (architecture §12.2; plan
//! `docs/plan/2026-11-06-m6-gap-audit.md` §4.3):
//!
//! - [`check`] (PK-win-upd-1): the scheduled and manual signed checks and
//!   the notification, in the service or the portable build, with the
//!   same window, policy, check request and trust file as on Unix, and
//!   which artifact this installation would take (the MSI or the portable
//!   zip). Install and Roll back answer `not_built_yet`.
//!
//! Later slices add their modules here: the MSI install through the
//! update helper (PK-win-upd-2), its rollback and resume (PK-win-upd-3),
//! and the portable build's own install (PK-win-portable-upd).
//!
//! It compiles and is tested on every OS; only the system side of
//! [`check::signals`] is Windows-only.

pub mod check;
pub mod helper;
pub mod history;
pub mod msi;
pub mod portable;
pub mod rollback;
