//! The Magnetico SQLite adapter (IMP-2, `docs/decisions/2026-10-30-IMP-2.md`):
//! reads a stopped magneticod database read-only, in keyset batches, into
//! IMP-1 import records.

#[cfg(test)]
pub(crate) mod fixtures;
