# WK-6: Results and Counters codecs: versioned, bounded, fuzzed, no pieces, 3 KB median
**Model:** Opus, with `/security-review` where the notes ask. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T15:20Z. Launched because soak r5 passed (`docs/decisions/2026-11-06-soak-r5-verdict.md`), which unheld this slice. The launch fits owner §2.42's four slots.

## What to build
- **The plan:** `docs/plan/2026-11-06-v06-workers.md` §4 (the WK-6 row and its slice notes). It's the spec; read the whole plan for context. The architecture sections it cites are "§19.2", "§19.4", "§19.8", "§3".
- **Its `runner/slices.yaml` entry** (`WK-6`), including its notes: /security-review (coordinator decodes bytes from workers); plan Q8. Shared: one mod line in pipeline lib.rs, merged in sequence with WK-0 and WK-7 (the runner orders the merges)
- **Owner decisions that bind it:** `docs/owner-decisions.md` §2.46 and §2.47 (the planning-wave answers). Where a plan row decided a question, the plan row is the decision.

## Own
- "crates/pipeline/src/wire/mod.rs"
- "crates/pipeline/src/wire/results.rs"
- "crates/pipeline/src/wire/counters.rs"
- "crates/pipeline/tests/wire.rs"
- "crates/pipeline/tests/fixtures/wire/**"
- "fuzz/fuzz_targets/cluster_results.rs"
- its tests and fixtures, a decisions file `docs/decisions/2026-11-06-WK-6.md`, and its own `runner/slices.yaml` entry (`owns` only, if it changes).

**Running beside it:** three other slices, with disjoint `owns`: WK-0, WK-6, WK-4 and X-consent-gate-052. If you need a file outside your `owns`, message the runner before touching it.

## Done when
- These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test` for the crates you touch and their dependants (per crate if the disk is short; say so), any fuzz target the plan names (10 minutes), `cd tools/status-connector && npm test`, and `/security-review` if the notes ask for it.
- No fixed sleeps: wait on a condition with a deadline of at least 10 s.

Branch `slice/WK-6`. Open the PR titled "WK-6: <a short imperative title, under 60 characters>". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
