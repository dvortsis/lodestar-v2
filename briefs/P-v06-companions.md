# P-v06-companions: plan the companion-manager epic (E-v06-companions) into slices
**Model:** Opus (docs and planning). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-05T23:50Z. The owner asked for planning work to start now. Building stays held for the soak verdict.

## Read first
- `docs/build-plan.md` Track K and P4 (companions);
- the companion sections of `docs/architecture.md` (search "companion");
- `docs/owner-decisions.md` (search "companion");
- what exists: likely nothing. Confirm with `git grep -i companion`.

Track K lists the parts:
- a signed catalogue, and checksum and signature checks;
- supervised child processes;
- encrypted credentials. These three are Opus work;
- compose and CA-template generation, detection adapters and the Companions UI. These are Sonnet work.

Security matters most here: running other programs, verifying what's downloaded, and keeping credentials. Mark every such slice `/security-review`. Put any trust-root or key-handling decision to the owner.

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

Plan file: `docs/plan/2026-11-06-v06-companions.md`. Decisions: `docs/decisions/2026-11-06-P-v06-companions.md`. Branch `slice/P-v06-companions`. PR title: "P-v06-companions: Plan the companion-manager slices".
