# X-enr-lane-nits (Sonnet, small)

Runner brief, 2026-11-20. Optional notes from the post-merge review of the runner's #971 commits. These are tidy-ups only: no behaviour change.

1. **fuzz/README.md** has no row for the `igdb` fuzz target, although fuzz/Cargo.toml has `[[bin]] name = "igdb"` and fuzz/fuzz_targets/igdb.rs exists. Add a row in the same style as its neighbours (tvdb, srrdb, predb). Read fuzz_targets/igdb.rs and say exactly what it checks: no panics, the caps, and what a kept value is. Don't overclaim.
2. **crates/sources-plugins/src/catalogue.rs ~line 39:** `("igdb", ...)` sits after `musicbrainz`, out of order. Every other entry, and `pub mod` in lib.rs, is alphabetical. Move it to after `full_scrape`. If a test or comment depends on the order (e.g. a stable listing order shown in the UI or a golden), update it, or keep the order and fix the comment that claims it's alphabetical. Say which in the PR.
3. **crates/sources-plugins/tests/credential_spec.rs:** the review called a comment near the per-plugin registration list (around the srrdb, anilist and openlibrary blocks, ~line 320–335) stale. Check that each comment names the right slice and plugin, and fix any that doesn't. If nothing is stale, say so in the PR. Don't invent a change.

## Owns
- fuzz/README.md (the igdb row only)
- crates/sources-plugins/src/catalogue.rs
- crates/sources-plugins/tests/credential_spec.rs (comments only)

## Checks
cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test -p lodestar-sources-plugins. ≲ 40 changed lines. No decisions file.
