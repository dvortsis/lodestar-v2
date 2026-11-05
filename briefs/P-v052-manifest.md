# P-v052-manifest: draft the v0.5.2 feature manifest (the freeze list for the cut)
**Model:** Opus (docs and planning). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-05T23:50Z.

## Why
`docs/build-plan.md` (search "v0.5.2") says to cut `release/v0.5.2` from `main` soon after v0.5.1 ships, and to **freeze the cut first**. The freeze needs a feature manifest:
- every behaviour enabled in the build, each with its own acceptance;
- every partly landed feature (Sources, rule packs, enrichment, M7, the crawler switches) marked inactive or excluded, with how it's kept off.

`main` diverges fast, so draft it now. The cut itself waits for the v0.5.1 tag (`ext:v051-tagged`).

## Read first
- the build-plan v0.5.2 paragraph and the P-v052-cut gate in `runner/slices.yaml`;
- the decisions in `docs/decisions/` since the v0.5.1 cut;
- `D-v052-notes`, the release notes already drafted;
- the settings defaults and the switches: search for the `enabled` and `off` defaults for sources, packs, enrichment, `dht.*` and `sources.verify_unverified`;
- the migrations V13+ and the search schema (v5 → v7).

## Do
1. Write `docs/plan/2026-11-06-v052-feature-manifest.md`:
   - **one table of enabled behaviours:** each with its acceptance (a test, gate or soak) and the PR that landed it;
   - **one table of inactive or excluded features:** each with the switch or default that keeps it off, and the test that proves it stays off;
   - **the one-way migrations and schema bumps**, with the V12 → V13+ upgrade smoke and the rollback-to-snapshot check the gate adds;
   - **the delta-rule re-soak triggers** (crawler lifecycle, sampler, endpoint policy) found in the diff since v0.5.1;
   - **open questions:** runner or owner, with recommendations.
2. Write `docs/decisions/2026-11-06-P-v052-manifest.md`.
3. Don't edit `runner/*.yaml`. Write no code.

## Done when
Every merged slice since the v0.5.1 cut is either in a table or listed as having no user-visible behaviour. A fresh self-review is done. Follow `.claude/skills/steward/SKILL.md`. Never merge. Branch `slice/P-v052-manifest`. PR title: "P-v052-manifest: Draft the v0.5.2 feature manifest". Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Times are UTC.
