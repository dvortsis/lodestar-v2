//! Benchmarks and the synthetic dataset generator (architecture §3, §13).
//!
//! [`synth::Generator`] produces a deterministic, endless stream of
//! realistic info dictionaries. It feeds the M0 size report and, later, the
//! 10M-torrent search benchmarks.

pub mod synth;
