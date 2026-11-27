# X-enr11-size-measure: Measure the size budget with identification answers in the corpus

**Runner brief, 2026-11-27T08:41Z.** Core, Opus (index and store sizing). Target < 500 changed lines.
- **Branch:** `slice/X-enr11-size-measure` from fresh `origin/main` (with #1132 merged as b70243a0).
- **PR title:** `X-enr11-size-measure: Measure the size budget with identification answers in the corpus`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- The `runner/slices.yaml` entry already exists (r18). Don't add another; edit nothing else in that file or `runner/dashboard.yaml`.

## Why
The runner's audit of #1110 (ENR-11) found the §3 index and store budgets unmeasured for version 3 claims. Each identified torrent can now carry up to 8 indexed alternative titles (the claim keeps up to 32) and ID terms (`id:imdb:tt…` and the other schemes). `bench/src/bin/size-report.rs` reports bytes per torrent against the owner-set budgets, but `bench/src/synth.rs` builds no identified torrents, so the report says nothing about them.

## Read first
Architecture §3 (budgets), `docs/decisions/2026-11-24-ENR-11.md`, `docs/decisions/2026-11-12-ENR-10.md`, `bench/src/synth.rs`, `bench/src/bin/size-report.rs`, `bench/src/fixture.rs`, how claims rows and the identification terms reach the index (`crates/store/src/identify_sweep.rs`, `crates/search/src/index.rs` `torrent_doc`), and `runner/standards.md`.

## Do
1. The synthetic corpus gains a realistic, configurable share of identified torrents (a version 3 claim with a match confidence, from a source whose terms allow serving it), with alternative titles in several scripts and lengths and one to three ID schemes each. Pick a default share and justify it in the decisions file (for example from ENR-10's expected coverage); also report 0 % and 100 % so the range is visible.
2. The report gives bytes per torrent with and without identification, by part (torrent index, file index, store), against the same budgets.
3. **No budget changes here.** If a budget no longer holds at the default share, say so in the PR and the decisions file, and message the runner; it's the runner's call under §2.26, or the owner's.
4. Keep the existing report's output stable for the 0 % case, so earlier figures stay comparable. Say how long the report takes at the corpus size CI uses.

## Owns
`bench/src/synth.rs`, `bench/src/bin/size-report.rs`, `docs/decisions/*-X-enr11-size-measure.md`. Not `bench/README.md`, `bench/src/bin/search-bench.rs` or `bench/tests/dataset.rs` (M8-PERF-1, #1107, holds them). If you need `bench/src/fixture.rs` or a store/search helper, message the runner first.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-bench` plus the crates you touched (a full workspace build may not fit the disk allowance; say what you ran). Put the report's numbers in the PR description. No fixed sleeps. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
