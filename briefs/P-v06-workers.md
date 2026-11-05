# P-v06-workers: plan the remote-workers epic (E-v06-workers) into slices
**Model:** Opus (docs and planning). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-05T23:50Z. The owner asked for planning work to start now. Building stays held for the soak verdict.

## Read first
- `docs/build-plan.md` Track W and P4 ("Remote workers v1");
- `docs/architecture.md` §19 (workers, the §19.8 gates) and §1.6.5 (the worker's ≤ 256 MB budget);
- `docs/plan/2026-11-03-soak-r3-memory-fix.md` and `docs/decisions/2026-11-03-soak-r3-verdict.md`. The fetch pool's 200k cap at ~690 B per key doesn't fit a worker's 256 MB, so size the worker pool cap;
- what's built: `crates/cluster/**` (CA, mTLS, join tokens, the postcard-over-WebSocket protocol, heartbeats, fencing), `crates/lodestar` (where `lodestar worker` goes), the DHT crate (for `sharded(keyspace)`), the fetch pool and `crates/pipeline`;
- the migration queue (§0.7).

## Do
1. Write the plan file named below, modelled on `docs/plan/2026-11-01-v06-sources.md` and `docs/plan/2026-11-04-v06-rulepacks.md`:
   - an inventory of what exists, against what the plan and architecture need, with file paths;
   - the slices, each with an id, model (Opus for parsers, untrusted input, on-disk formats, concurrency, security and core, per CLAUDE.md), a size estimate (≲ 1,500 lines each), deps and disjoint `owns`, in `runner/slices.yaml` one-line flow style, ready to paste;
   - gates;
   - open questions, each with the runner's recommendation, marked **owner** or **runner** per CLAUDE.md. Anything that loosens an owner decision, weakens security or content safety, or needs the owner's hands is the owner's;
   - waves and the critical path. Slots are limited to 4 from 2026-11-06T00:00Z, and building waits for the v0.5.1 soak verdict (r5, baseline ends 2026-11-06T13:12Z).
2. Write a decisions file for your choices.
3. Don't edit `runner/slices.yaml` or `runner/dashboard.yaml`. The runner adds the slices after review.
4. **Planning only: no code.** Read the code to inventory it.

## Done when
The plan is self-consistent:
- every dep names an existing or planned id;
- parallel slices' `owns` don't overlap;
- sizes are ≲ 1,500 lines.

A fresh self-review is done. Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Times are UTC. Work frugally.

Plan file: `docs/plan/2026-11-06-v06-workers.md`. Decisions: `docs/decisions/2026-11-06-P-v06-workers.md`. Branch `slice/P-v06-workers`. PR title: "P-v06-workers: Plan the remote-workers slices".
