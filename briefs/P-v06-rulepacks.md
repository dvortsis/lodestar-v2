# P-v06-rulepacks: plan the curated rule packs and the ThePornDB/StashDB enrichment slices
**Model:** Opus (docs and planning, no code). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
E-v06-sources has two halves. P-v06-sources-plan planned the sources half (`docs/plan/2026-11-01-v06-sources.md`). This slice plans the other half the same way: curated rule packs, and ThePornDB/StashDB enrichment.

## Read first
- `docs/plan/2026-11-01-v06-sources.md` (the model to copy: inventory, slices with owns/deps/size, open questions with recommendations, waves).
- `docs/architecture.md` §20.6 and §20 generally (filters and rule packs), §18.2 (enrichment), and §1.8 (adult content and consent).
- `docs/owner-decisions.md` §2.8 and §2.16–§2.21, and build-plan P4 "Filters".
- The filter code as it stands: `crates/filters/**` (the packs from F-filters-packs, fail-closed loading from #357, stand-ins from #367).

## Do
1. Write **`docs/plan/2026-11-04-v06-rulepacks.md`**:
   - what exists against what P4 and §20.6 need;
   - the slices, each with an id, model, size estimate, deps and disjoint `owns`, in `runner/slices.yaml` one-line flow style, ready to paste;
   - gates;
   - open questions, each with the runner's recommendation, marked owner or runner per CLAUDE.md. Content-safety and adult-content scope go to the owner;
   - waves and the critical path.
2. Write **`docs/decisions/2026-11-04-P-v06-rulepacks.md`** for your choices.
3. Don't edit `runner/slices.yaml` or `runner/dashboard.yaml`. The runner adds the slices after review.

## Done when
The plan is self-consistent:
- every dep names an existing or planned id;
- no two parallel slices' `owns` overlap;
- the slice sizes are ≲ 1,500 lines.

A fresh self-review is done.

Branch `slice/P-v06-rulepacks`. Open the PR titled "P-v06-rulepacks: Plan the curated rule packs and ThePornDB/StashDB enrichment slices". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally.
