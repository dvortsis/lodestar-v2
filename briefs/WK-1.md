# WK-1: Coordinator wiring in lodestar serve: [cluster] config, port off until allowed, PKI, reseal, metrics
**Model:** Opus, with `/security-review`. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T18:10Z. It launches into the slot WK-0 freed (#538 merged at `62ec1128`), within owner §2.42's four slots, and sits on the workers critical path (WK-1 → WK-2 → …).

## What to build
- **The spec:** `docs/plan/2026-11-06-v06-workers.md` §4, the WK-1 row and Q3 (the cluster port isn't bound until "Allow workers" is on; the default stays 8091; a tightening). Read the whole plan. The architecture sections it cites are §19.2, §19.6 and §12.
- **Its `runner/slices.yaml` entry (`WK-1`), including its notes:**
  - `/security-review`;
  - shared one-block edits to `services.rs`, `config.rs` (`[cluster]` section), `main.rs` and `docs/metrics.md` (the `lodestar_cluster_*` rows).
- **What WK-0 left for you** (`docs/decisions/2026-11-06-WK-0.md`):
  - `ClusterServer::with_hooks(Arc<LeaseAuthority>, …)`, built only once workers are allowed, so a single-machine install opens no cluster CA and binds no port;
  - `Monitor::lease_authority()`;
  - the byte-capped queues;
  - `GrantAdmission`.
- **Config validation, from #538's review:** validate `max_skew_ms` (and every new `[cluster]` number) to a sane positive range at load. The server's `max_skew_ms.abs()` panics on `i64::MIN`, so out-of-range values must be refused before they reach it, with a clear error.
- **Owner decisions that bind it:**
  - `docs/owner-decisions.md` §2.46 and §2.47: wkq1 (what a worker holds) and wkq2 (`LODESTAR_TOKEN` for the first join only; that's WK-2's, but don't preclude it);
  - §1.6.5.

  Where a plan row decided a question, the plan row is the decision.

## Own
- `crates/lodestar/src/cluster/mod.rs`, `crates/lodestar/src/cluster/serve.rs`, `crates/lodestar/tests/cluster_serve.rs`;
- the one-block shared edits named above;
- its tests;
- a decisions file, `docs/decisions/2026-11-06-WK-1.md`;
- its own `runner/slices.yaml` entry (`owns` only).

**Running beside it:** WK-6 (#539, in review), ENR-9 (#542, in review) and CMP-0a, which adds one dependency line to `crates/lodestar/Cargo.toml` and `CATALOGUES`/`TARGETS` lines to `crates/lodestar/src/logging/mod.rs`. Keep `owns` disjoint. Message the runner before touching any file outside your `owns`. For `Cargo.toml` and `logging/mod.rs`, whichever PR merges second brings `main` in.

## Done when
- **These pass:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test` for the crates you touch and their dependants (run per crate if the disk is short, and say so);
  - `cd tools/status-connector && npm test`;
  - `/security-review`.
- **A real-binary test proves the single-machine guarantees:**
  - with workers off, no cluster port is bound and no cluster CA or settings rows are created;
  - turning workers on binds 8091 (or the configured port).
- **No fixed sleeps.** Wait on a condition with a deadline of at least 10 s.

Branch: `slice/WK-1`. Open the PR titled "WK-1: <a short imperative title, under 60 characters>". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
