# RP-5: SP-4, clones of hidden spam (payload-signature set, inherited action, release follows the original)
**Model:** Opus (filter and content-safety core). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T19:20Z. Runs in the same session as RP-4.

## Why
RP-4 (#495, merged 85179f32) added SP-3 flooding. SP-4 is the last spam rule. When an original is hidden by SP-2, or by SP-3 at demote or above, its payload signature joins a capped set. A later torrent with the same payload signature then inherits the original's action. The spec is in `docs/plan/2026-11-04-v06-rulepacks.md`: §4.3 "RP-5", line 47 (storage), and Q12 (the runner's call: 1M signatures, 8 MB, oldest dropped first, in append-only 64 KB `settings` chunks). This is the gate before RP-G.

## Do (branch `slice/RP-5` from `origin/main`)
1. **The set** goes in `crates/filter/src/spam_sigs.rs`, with storage via `load.rs`.
   - Store packed 8-byte signatures in append-only `settings` chunk rows of at most 64 KB (`filter.sp4.sigs.<n>`). One hide writes one small row, never the whole set.
   - Releases mark removals. Compaction rewrites a chunk only once a quarter of it has been removed.
   - The cap is 1M; the oldest signature is dropped first.
   - The set loads at start, and it fails closed: an unreadable chunk loses only that chunk's signatures, it doesn't make them all pass, and it's listed in `filter.stand_ins` with a notice, the same way RP-0 handles lost state.
2. **The rule** goes in `crates/filter/src/packs/sp4.rs`.
   - The engine checks the set inside the pack layer's precedence, the same way RP-4's entry point does. Every SP-3 precedence test has an SP-4 twin: CSX-5 Override, exception, *Always allow*, switches, user rule, pack override, strongest wins.
   - A clone inherits the original's action. Record the action with the signature, or look it up. Say which in the decisions file.
   - Add SP-4@1 to the manifest as `widens`, listed in `ADDED_AFTER_RP0`, so it's pending on upgraded stores and applied on fresh ones. Add its CHANGELOG entry.
3. **The writer** (`crates/pipeline/src/writer.rs`). When SP-2, or SP-3 at demote or above, hides a torrent at commit, add its payload signature (`payload.rs`).
   - Keep the hot path cheap. Measure against RP-4's `sp3_writer_cost` and stay inside the 5% gate for crawl and feed commits. Move `sp3_writer_cost`, plus an SP-4 equivalent, into the `bench/` gate if it's cheap to do, since that's a recorded follow-up.
4. **Release follows the original.** An exception for the original, or a release through the release path in `crates/api/src/filters.rs` (the release hook only), removes its signature, so clones follow. Test both, release-follows and exception-follows.
5. **Gates and tests:**
   - a clone fixture;
   - release-follows and exception-follows;
   - the cap test: the oldest is dropped at 1M; a smaller test cap is fine if the code path is the same;
   - the set survives a restart;
   - one hide writes exactly one chunk row;
   - compaction at a quarter removed;
   - fail-closed chunk loss;
   - the filter budget (`props.rs`) still holds with the set loaded at its cap, because RP-G will check that;
   - no fixed sleeps.
6. **Interactions:**
   - ENR-1c (#502, in review) edits `writer.rs` lightly. Bring `main` in if it merges first.
   - SP-4 must not be fed by enrichment or torrent-cache claims, the same exclusions RP-4 settled. Only real hides of stored torrents count.

## Own
`crates/filter/src/packs/sp4.rs`, `crates/filter/src/spam_sigs.rs`, `crates/filter/tests/sp4.rs`, and the SP-4 hunks in `engine.rs`, `load.rs`, `writer.rs`, `api/filters.rs` (release hook only), the manifest and CHANGELOG, plus the API goldens (re-blessed only for SP-4 text) and `docs/decisions/2026-11-05-RP-5.md`.

## Done when
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- the filter budget
- `/security-review` on the diff

Then open the PR "RP-5: SP-4 clones of hidden spam (payload-signature set, inherited action, release follows the original)" with the label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner with the PR number and head. Never merge. Push once per round. No fixed sleeps.
