# GS-gate: Size gate in ci-ok: size-report --gate on every search, classify and bench change
**Model:** Opus. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T19:20Z. It launches into the slot WK-6 freed (#539 merged at `5a41b748`), within owner §2.42's four slots. It's small (about 150 lines, plan estimate) and turns the index-size budgets into a CI gate before the v0.7 producers (GS-0 and others) land.

## What to build
- **The spec:** `docs/plan/2026-11-05-v07-m7.md`, the GS-gate row (about line 69) and Q10 (the 512 B producer budget). Read the whole plan for context. The architecture section it cites is §3 (budgets), plus build-plan Track M7.
- **Its `runner/slices.yaml` entry (`GS-gate`), including its notes:**
  - it owns `INDEX_BUDGET` and `FILE_INDEX_BUDGET`;
  - the size-budget job's path filter widens to `crates/search/**`, `crates/classify/**` and `bench/**`;
  - `ci-ok` needs that job, so a producer over 512 B turns CI red.
- **Owner numbers:** the budgets are owner-set (§3, §10.1). Don't change any of them. This slice only enforces them. The runner may tune them later under §2.26, and a slice may not.
- **Make the gate fail closed, and say why it failed.** If `size-report` can't measure, the job fails. Show the budget and the measured value in the job summary. Check that a PR touching none of the filtered paths still passes `ci-ok`, whether the job is skipped as success or not needed.
- **Prove it** with a branch-local demonstration: temporarily lower a budget or add an oversized producer, show the gate going red, then revert. Report the run IDs in the PR.

## Own
- `.github/workflows/ci.yml`;
- `bench/src/bin/size-report.rs`;
- a decisions file, `docs/decisions/2026-11-06-GS-gate.md`;
- its own `runner/slices.yaml` entry (`owns` only).

**Running beside it:**
- WK-1 (lodestar binary and `crates/cluster/src/server.rs`);
- WK-7 (pipeline);
- CMP-0a (#543, companions).

None touches CI workflows. Message the runner before touching any file outside your `owns`.

## Done when
- `cargo fmt --check` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo run -p lodestar-bench --bin size-report -- --gate` passes locally, or with the right package name.
- `actionlint` passes, if available.
- `cd tools/status-connector && npm test` passes.
- The red-then-green demonstration is reported in the PR.

Branch `slice/GS-gate`, with the PR titled "GS-gate: <a short imperative title, under 60 characters>". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round, except for the demonstration push, which you revert. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
