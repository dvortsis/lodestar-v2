# P-m6-audit: the M6 packaging gap audit (E-M6) and its slices
**Model:** Opus (docs and planning). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-05T23:50Z. The owner asked for planning work to start now. Building stays held for the soak verdict.

## Read first
- `docs/build-plan.md` Track M6 and P3, and `docs/architecture.md` §14 (M6) and the packaging sections;
- what's built: the clap CLI (`backup`, `rollback`, `vacuum`, `reindex`, `stats`), the Windows service, the WiX MSI and portable zip, the Unraid templates (main, gluetun, worker), the systemd unit, the tarballs, `release.yml`, the smoke jobs, and `docs/user/`;
- `docs/owner-decisions.md` §2.23 (unsigned Windows builds) and §2.37 (the CA submission after 1.0).

## Audit
Audit P3 and §14 item by item: built, partly built or missing, with evidence (file:line, a workflow job, a test). The known items:
- **FUSE and network-filesystem detection:** does it exist?
- **The worker template:** label it "requires v0.6" more prominently, or keep it out of the CA submission until Track W step 1.
- **The user guide's v0.5.1 refresh:** the slow first hours caused by the 6 h sampling suppression (`docs/decisions/2026-10-31-owner-delegation.md`). The guide follows the `user-docs` skill.
- Anything else you find.

The CA submission is the owner's, after 1.0; it's not part of M6's exit.

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

Plan file: `docs/plan/2026-11-06-m6-gap-audit.md`. Decisions: `docs/decisions/2026-11-06-P-m6-audit.md`. Branch `slice/P-m6-audit`. PR title: "P-m6-audit: M6 packaging gap audit and slices".
