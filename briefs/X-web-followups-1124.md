# X-web-followups-1124: List view keeps its active row, and the companion route editor's follow-ups

**Runner brief, 2026-11-24T23:40Z.** Helper, Sonnet, web only. Target < 600 changed lines. **Start after X-gs-followups (#1121) merges**, because it edits ResultsPanel.svelte.
- **Branch:** `slice/X-web-followups-1124` from fresh `origin/main`.
- **PR title:** `X-web-followups-1124: List view keeps its active row; companion route editor follow-ups`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- Add this slice's own `runner/slices.yaml` entry (kind helper, model sonnet, the owns below, notes "from #1115 and rv-1114") and a placement line in `runner/dashboard.yaml` beside X-purge-rederive's. Run `cd tools/status-connector && npm test`.

## Read first
`CLAUDE.md` UI rules (tokens only, the §10.2 spacing ramp, smallest UI first), `docs/architecture.md` §10.5 and §10.14, `docs/design-tokens.md`, `docs/ui-glossary.md` and `runner/standards.md`. Also #1115 (X-flake-search-keyboard-2), which fixed the same reset in the table, and `git show origin/runner-state:reviews/rv-1114.md`.

## Do
**A. List view active row (from #1115):** the list view has the same active-row reset problem that #1115 fixed in the table (`resetKey` in `web/src/lib/ui/lists/VirtualList.svelte`). Fix it the same way, so the active row survives a re-render that doesn't change the result set. Add a unit test that fails without the fix, in `web/src/lib/ui/lists/lists.test.ts` or beside the harness. Keyboard behaviour stays as the table's.

**B. Companion route editor (rv-1114, in `web/src/lib/features/settings/companions/`):**
1. Route.svelte re-fills from `c.egress` when the server value changes and the form is untouched. A stale form must not revert a newer route.
2. In egress.ts `routeText`, say bind_address explicitly, and give unknown kinds a neutral sentence.
3. Comment the case where the component unmounts mid-PUT, in Route.svelte or confirm.ts.
4. CompanionRow: routeRefused clears an older note, and the panel resets when routeOffered goes false.

Each item gets a test. No API change: the follows list is a separate slice (X-cmp-egress-follows).

## Owns
`web/src/lib/ui/lists/VirtualList.svelte`, `web/src/lib/ui/lists/lists.test.ts`, `web/src/lib/ui/lists/testing/**`, `web/src/lib/features/search/cards/CardList.svelte` (only if the fix needs it), `web/src/lib/features/settings/companions/**`, `web/.size-limit.mjs` (only the entries this slice changes), the specs and snapshots it touches, `docs/decisions/2026-11-24-X-web-followups-1124.md`, and your slices.yaml entry and placement line. Not `web/src/lib/features/upload/**` or the settings shell, which belong to UP-2 #1106.

## Done
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and `npx playwright test` for the specs you touch, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` (never `playwright install`). No fixed sleeps. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green, and say whether any snapshots or size entries changed. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
