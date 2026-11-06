# X-consent-gate-052: Refuse to enable a source that needs consent until SRC-15b ships owner-approved consent text
**Model:** Opus, with `/security-review` where the notes ask. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T15:20Z. Launched because soak r5 passed (`docs/decisions/2026-11-06-soak-r5-verdict.md`), which unheld this slice. The launch fits owner §2.42's four slots.

## What to build
- **The plan:** `docs/plan/2026-11-06-v052-feature-manifest.md` §3.4. It's the spec; read the whole plan for context. The architecture sections it cites are "docs/plan/2026-11-06-v052-feature-manifest.md §3.4".
- **Its `runner/slices.yaml` entry** (`X-consent-gate-052`), including its notes: The check goes in Control::update_source's enable path (consent::need_in under the store lock), not the API layer; crates/api/src/ops.rs and crates/lodestar/src/ops.rs only if a new error code needs mapping; after X-tcache-recheck, which owns control.rs disable paths; /security-review; runner call on P-v052-manifest Q4; lands before P-v052-cut. PATCH /sources/{id} refuses, with a clear error, to enable any source whose consent::need isn't None; a test against the real binary. owns proposed by the runner (the plan names none); narrowed at launch
- **Owner decisions that bind it:** `docs/owner-decisions.md` §2.46 and §2.47 (the planning-wave answers). Where a plan row decided a question, the plan row is the decision.

## Own
- "crates/pipeline/src/control.rs"
- "crates/pipeline/tests/control.rs"
- "crates/lodestar/tests/consent_gate.rs"
- its tests and fixtures, a decisions file `docs/decisions/2026-11-06-X-consent-gate-052.md`, and its own `runner/slices.yaml` entry (`owns` only, if it changes).

**Running beside it:** three other slices, with disjoint `owns`: WK-0, WK-6, WK-4 and X-consent-gate-052. If you need a file outside your `owns`, message the runner before touching it.

## Done when
- These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test` for the crates you touch and their dependants (per crate if the disk is short; say so), any fuzz target the plan names (10 minutes), `cd tools/status-connector && npm test`, and `/security-review` if the notes ask for it.
- No fixed sleeps: wait on a condition with a deadline of at least 10 s.

Branch `slice/X-consent-gate-052`. Open the PR titled "X-consent-gate-052: <a short imperative title, under 60 characters>". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
