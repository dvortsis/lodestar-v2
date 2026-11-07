//! G2 and G3 of the v0.5.2 manifest
//! (`docs/plan/2026-11-06-v052-feature-manifest.md` §3.3): the pipeline's
//! own defaults keep adult enrichment unwired and the verification cursor
//! off. `crates/lodestar/tests/release_defaults.rs` checks that the binary
//! leaves both defaults alone.

use lodestar_pipeline::PipelineConfig;

/// G2: no plugin may attach an adult-enrichment section unless the host
/// names it, and by default it names none (ENR-1c).
#[test]
fn no_adult_enricher_is_named_by_default() {
    assert!(
        PipelineConfig::default().adult_enrichers.is_empty(),
        "v0.5.2 ships no adult-enrichment source"
    );
}

/// G3: `sources.verify_unverified` stays off until C8 ends
/// (build-plan §0.2).
#[test]
fn the_verification_cursor_is_off_by_default() {
    assert!(!PipelineConfig::default().verify_unverified);
}
