# X-filter-perf-1105: win back filter-budget headroom
**Model:** Opus (filter engine hot path). A fresh Opus reviewer follows. Runner brief, 2026-11-05T08:58Z.

## Why
RP-UC (#469) left the filter budget with almost no headroom:
- calibrated mixed: 48.2–49.6 µs, against 50 (main before RP-UC: 46.0–48.8);
- the 10% Fuzzy case: 49.9–50.9 µs. It's reported but not gated, so it sometimes reads over 50 in practice.

RP-1b, RP-4 and RP-5 will add engine work next. Win back headroom now, without changing any verdict.

## Do (branch `slice/X-filter-perf-1105` from `origin/main`)
1. **Profile** the budget bench with `perf` or `cargo flamegraph`, using the release profile and the reference runner settings in the bench README: mixed, calibrated and fuzzy. Find the hot paths: UC-3's scene-name path, `allowed()`, release parsing per item, title keys, per-item allocation, regexes and repeated lowercasing.
2. **Optimise with zero behaviour change.**
   - Cache per-item derived data computed once (parsed release, lowercased name, tokens) and share it across rules.
   - Remove allocations in the per-item path.
   - Precompile what can be compiled.
   - Short-circuit where a rule's cheap preconditions fail.
3. **Prove there's no change.** All of these must stay identical:
   - every corpus, the precision set and the goldens;
   - the 3,000-combination GOLDEN test;
   - the pack fixtures;
   - `cargo test -p lodestar-filter -p lodestar-classify`.
4. **Report before and after on the same machine,** at least 3 runs each: raw mixed, calibrated mixed, 10% Fuzzy and the worst case. **Target:** calibrated mixed ≤ 45 µs and Fuzzy ≤ 47. Report what you reach, even if it falls short.
5. **Decisions file:** `docs/decisions/2026-11-05-X-filter-perf-1105.md`.

## Own
`crates/filter/src/engine.rs`, `crates/filter/src/load.rs`, `crates/filter/src/packs/unauthorised_copy.rs` (performance only, not matching), `crates/classify/src/**` hot paths (performance only), and the filter bench under `crates/filter/tests/props.rs` (measurement only, never the limit).

Don't touch rule definitions in `crates/filter/src/packs/mod.rs` (RP-1b owns them now) or `crates/api/**` (RP-3).

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the filter and classify tests and the release budget pass. Open the PR against `main` titled "X-filter-perf-1105: win back filter-budget headroom". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, head and before/after table, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
