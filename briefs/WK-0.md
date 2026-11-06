# WK-0: One lease authority: standalone LeaseAuthority held by the pipeline; every v1 protocol message; inbound sink, grant admission, byte-capped queues
**Model:** Opus, with `/security-review` where the notes ask. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T15:20Z. Launched because soak r5 passed (`docs/decisions/2026-11-06-soak-r5-verdict.md`), which unheld this slice. The launch fits owner §2.42's four slots.

## What to build
- **The plan:** `docs/plan/2026-11-06-v06-workers.md` §4 (the WK-0 row and its slice notes). It's the spec; read the whole plan for context. The architecture sections it cites are "§19.2", "§19.3", "§10.9.3".
- **Its `runner/slices.yaml` entry** (`WK-0`), including its notes: /security-review; plan Q4. Adds peer hints on ClaimItem, the hint relay, chunked filter-snapshot and re-verification messages, ClusterServer gets an Arc to the authority only when workers are allowed, so a single-machine install never opens a cluster CA. Later proto.rs or server.rs changes are real edits (allowed until WK-G, plan Q11) and go to the runner. Single-machine parity bench before and after; crates/cluster tests and the cluster_frame fuzz target stay green
- **Owner decisions that bind it:** `docs/owner-decisions.md` §2.46 and §2.47 (the planning-wave answers). Where a plan row decided a question, the plan row is the decision.

## Own
- "crates/cluster/src/server.rs"
- "crates/cluster/src/proto.rs"
- "crates/cluster/src/client.rs"
- "crates/cluster/src/leases.rs"
- "crates/cluster/src/tests.rs"
- "crates/pipeline/src/coordinator.rs"
- "crates/pipeline/src/lib.rs"
- "crates/pipeline/tests/lease_authority.rs"
- its tests and fixtures, a decisions file `docs/decisions/2026-11-06-WK-0.md`, and its own `runner/slices.yaml` entry (`owns` only, if it changes).

**Running beside it:** three other slices, with disjoint `owns`: WK-0, WK-6, WK-4 and X-consent-gate-052. If you need a file outside your `owns`, message the runner before touching it.

## Done when
- These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test` for the crates you touch and their dependants (per crate if the disk is short; say so), any fuzz target the plan names (10 minutes), `cd tools/status-connector && npm test`, and `/security-review` if the notes ask for it.
- No fixed sleeps: wait on a condition with a deadline of at least 10 s.

Branch `slice/WK-0`. Open the PR titled "WK-0: <a short imperative title, under 60 characters>". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
