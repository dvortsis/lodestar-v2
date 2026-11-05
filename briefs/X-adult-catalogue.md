# X-adult-catalogue: one compiled-in manifest catalogue, so the import CLI can never miss an always-adult source
**Model:** Opus (content safety). A fresh Opus reviewer follows. Runner brief, 2026-11-05T18:55Z.

## Why
This is #494's follow-up for "gap 4". Today `lodestar import` builds its always-adult mask from a hand-written list: `crates/lodestar/src/cli/import.rs:806-825` lists `nyaa::Sukebei` alone. The pipeline builds its mask from the plugins that are actually registered (`crates/pipeline/src/source_adult.rs`, `AdultSources::of`). So a second always-adult plugin added later would be masked in the pipeline but not on import, and its torrents would import as non-adult. Separately, the two "no manifest is both Enrichment and always_adult" tests (in `crates/sources-plugins/tests/tpdb.rs`, and `stashdb.rs` once #506 merges) find manifests by scanning source text, which is fragile.

## Do (branch `slice/X-adult-catalogue` from `origin/main`)
1. **The catalogue.** In `crates/sources-plugins`, add one function, for example `pub fn manifests() -> Vec<&'static Manifest>` or an iterator, that returns the manifest of every plugin compiled into this crate. It doesn't matter whether a plugin is registered; ThePornDB and StashDB, which are on HOLD, are included too. Put it in a small new module, `crates/sources-plugins/src/catalogue.rs`, plus one `mod` line in `lib.rs`.
   - A plugin that needs config to build its manifest (for example the torrent cache's endpoints) should expose a config-free static manifest for its fixed fields, or the catalogue documents how it gets one. The fields that matter here are `id`, `kinds` and `always_adult`.
   - Write a test that fails if a plugin module exists but is missing from the catalogue. A simple way is to count the `pub mod` plugin lines in `lib.rs` against the catalogue, with an explicit allow-list for modules that aren't plugins (helpers, `catalogue` itself). Keep the test readable.
2. **The import CLI uses it.** In `adult_sources` (`cli/import.rs`), take the always-adult manifests from the catalogue instead of naming Sukebei. Behaviour stays the same: the store's registered rows are matched by plugin id, and a disabled or purged source still contributes no bit (SRC-14b, owner flag gap 3, unchanged).
   - Test: a fake or extra always-adult manifest in the catalogue reaches the import mask. If the catalogue can't be faked, test that the CLI's set equals `catalogue().filter(always_adult)`.
   - Keep the existing `start_stores_a_hash_sukebei_claims_as_adult` passing.
3. **One manifest-safety test.** In `crates/sources-plugins/tests/manifests.rs`, iterate over the catalogue and assert:
   - no manifest is both `Kind::Enrichment` and `always_adult`;
   - ids are unique;
   - `always_adult` plugins are exactly the expected set, Sukebei today. A new always-adult plugin then has to update this test on purpose.

   Remove the source-text scan from `tests/tpdb.rs`, keeping its plugin-specific trait assertion. If `tests/stashdb.rs` is on `main` when you start, trim its copy the same way. If it isn't, tell the runner and ENR-5 will trim it.
4. **Decisions file:** `docs/decisions/2026-11-05-X-adult-catalogue.md`.

## Own
- `crates/sources-plugins/src/catalogue.rs`, plus one `mod` line in `crates/sources-plugins/src/lib.rs`
- `crates/sources-plugins/tests/manifests.rs`
- the scan-test lines in `crates/sources-plugins/tests/tpdb.rs` (and `stashdb.rs`, if it's on `main`)
- the `adult_sources` function in `crates/lodestar/src/cli/import.rs` and its test in `cli/import/tests.rs`
- the decisions file

**Shared:** if a plugin needs a static-manifest accessor, add one small `pub fn` or `const` in that plugin's file. Name each one in the PR.

**Don't touch:**
- `crates/pipeline/**` (ENR-1c and RP-4 are running there);
- `crates/lodestar/src/ops.rs` (SRC-11c);
- `crates/api/**`.

## Done when
Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` (or at least the `sources-plugins` and `lodestar` crate tests if disk is short; say which).

Open the PR "X-adult-catalogue: One manifest catalogue for the import CLI's always-adult mask", label `full-ci`. Add the `runner/slices.yaml` entry and its placement. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
