# X-gs-followups: Related and grouped results follow-ups (GS-4 and GS-5 reviews)

**Runner brief, 2026-11-24T21:15Z.** Helper, Sonnet, web only. Target < 800 changed lines.
- **Branch:** `slice/X-gs-followups` from fresh `origin/main` (GS-4 #1116 merged as 760dbc17).
- **PR title:** `X-gs-followups: Related and grouped results follow-ups`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- Add this slice's own `runner/slices.yaml` entry (kind helper, model sonnet, the owns below, notes "from rv-1108 and rv-1116"), and run `cd tools/status-connector && npm test`.

## Read first
`docs/architecture.md` §10.5 and §10.14, `docs/design-tokens.md`, `docs/ui-glossary.md`, `runner/standards.md`, and the GS-4 and GS-5 decisions files. UI rules in CLAUDE.md (tokens only, the §10.2 spacing ramp, smallest UI first).

## Do
From GS-4 (#1116):
1. **One /siblings request per torrent:** Parsed and Related share it (a small shared loader or cache in the detail feature). Stale answers are still dropped when the content settings change, and a wrong-hash answer still counts as a failure.
2. **The drawer hands Related an openDetail** (context, or an event through DetailHostBody/open.ts), so the on-demand open.ts import leaves the /t and Detail sections chunks. Measure with `npm run size:ci`. If those entries fall, lower their size-limit entries back toward where they were before GS-4, and record the numbers in your decisions file.
3. **GS-5's Show all opens Related directly** (open.ts/DetailHost and search/collapse).

From GS-5 (#1108):
4. Show all N either counts max(others, siblings)+1 or drops the number. Don't show a count that can be wrong.
5. Re-announce group loads that have the same text (clear, then set).
6. Carry the active row across a view switch by hash, not by index.
7. Nits: the test fixture at ResultsPanel.group.test.ts:247, move() at the end of the list, a duplicate data-testid, and GroupToggle being one-way.

## Owns
`web/src/lib/features/detail/**`, `web/src/lib/features/search/collapse/**`, `web/src/lib/features/search/results/ResultsPanel.svelte`, `web/src/lib/features/search/results/ResultsPanel.group.test.ts`, `web/.size-limit.mjs` (only the entries this changes), the specs and snapshots these touch, `docs/decisions/2026-11-24-X-gs-followups.md`, and this slice's `runner/slices.yaml` entry.

## Done
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and `npx playwright test` for the specs you touch, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` (never `playwright install`). No fixed sleeps. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green, and say in it whether snapshots or size entries changed. Push once per round of fixes.

Watch: `DetailActions.test.ts` failed 3 tests once in a local run (possible existing flake). If you see it, tell the runner. Don't fix it in this slice.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
