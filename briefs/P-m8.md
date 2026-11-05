# P-m8: plan the M8 hardening-to-1.0 epic (E-M8) into slices and gates
**Model:** Opus (docs and planning). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-05T23:55Z. The owner asked for more planning work now.

## Read first
- `docs/build-plan.md` P7, §0.6 (the 50M numbers) and the 1.0 gates row;
- `docs/architecture.md` §17 (the 1.0 gates), §3 (budgets) and §10.1;
- `docs/owner-decisions.md`;
- what exists for each gate:
  - the `relevance` bin and its judgment set (`relevance --gate`, currently 0.9186);
  - `search-bench` and the 10M results in `docs/decisions/2026-11-05-X-search-files-5.md`;
  - the chaos harness (`crates/cluster` tests);
  - the soak tools (`docs/soak/`);
  - the security reviews done so far (`/security-review` notes in `docs/decisions/`);
  - the theme set (48 themes) and the Playwright and axe jobs.

## Scope
P7's items:
- the relevance judgment set and NDCG@10 gate;
- the 10M performance and size gates on a dedicated runner, plus the 50M import-scale numbers;
- the crawler gate re-measured on the release candidate;
- the Windows soak;
- a full-system security review (plan it in parts by surface, each a slice);
- the plugin politeness and purge audits;
- the chaos gates;
- the CI wiring;
- the docs (`query-language.md`, `relevance.md`, `themes.md`, the user guide, the README);
- Playwright and axe across all 48 themes on a schedule.

Say which gates need the owner's hands or hosts (dedicated runner, Windows, the import host).

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

Plan file: `docs/plan/2026-11-06-m8-hardening.md`. Decisions: `docs/decisions/2026-11-06-P-m8.md`. Branch `slice/P-m8`. PR title: "P-m8: Plan the M8 hardening-to-1.0 slices and gates".
