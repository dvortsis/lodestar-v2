# X-gs-followups-2: Related and Parsed follow-ups: tests for the kept answer, focus after Show all, and the table's active row

**Runner brief, 2026-11-25T00:20Z.** Helper, Sonnet, web only. Target < 500 changed lines. Start once the runner frees slot 2 (after #1122 merges); nothing it depends on is open.
- **Branch:** `slice/X-gs-followups-2` from fresh `origin/main`.
- **PR title:** `X-gs-followups-2: Related and Parsed follow-ups: kept-answer tests, focus after Show all, table active row`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- Add this slice's own `runner/slices.yaml` entry (kind helper, model sonnet, the owns below, notes "from rv-1121") and a placement line in `runner/dashboard.yaml` if no rule covers it. Run `cd tools/status-connector && npm test`.

## Read first
`CLAUDE.md` UI rules (tokens only, the §10.2 spacing ramp, smallest UI first), `docs/architecture.md` §10.5 (focus and announcements) and §10.14, `docs/ui-glossary.md`, `runner/standards.md`, X-gs-followups' decisions file, and `git show origin/runner-state:reviews/rv-1121.md`.

## Do
1. **Tests (should-fix):** a unit test that a kept siblings answer expires after `FRESH_MS` (`detail/siblings.ts:81`, fake timers), and one that `markClosed()` drops the section wish (`detail/open.ts:58`; assert `wanted()` is null after a close).
2. **Focus after Show all** (`DetailHostBody.svelte:176-186`): once Related has mounted and scrolled into view, move focus to its heading (`tabindex=-1`, as Retry already does), so a keyboard or screen-reader user lands there. If §10.5 says the title keeps focus, record that in your decisions file instead and say why. Test it either way.
3. **Nits:**
   - `siblings.ts:51`: wrap each listener call so one that throws doesn't stop the others (or fix the comment at :54-56 to match).
   - `siblings.ts:79-82`: refresh a kept answer's LRU position on read.
   - `Parsed.svelte:32`: a `withheld` (404) answer hides the spans quietly, as Related does, instead of an error with Retry. Never show more than before.
   - `expand.svelte.ts:84-86`: check `r.hash === hit.hash` and treat a mismatch as a failure, as siblings.ts does. Test it.
   - `search/table/ResultsTable.svelte:96-99`: stop resetting `active` on mount, so the carried row survives List→Table without the re-set in ResultsPanel. Drop the `carried` re-set if it is now dead, and report the Search results (lazy) size change (rv-1121 expected ~30-40 B saved).
   - Add the Table→List and Table→Cards carry cases next to the existing List→Table test.

## Owns
`web/src/lib/features/detail/**`, `web/src/lib/features/search/table/ResultsTable.svelte`, `web/src/lib/features/search/results/ResultsPanel.svelte` (only the carried re-set) and `ResultsPanel.group.test.ts`, `web/.size-limit.mjs` (only entries this slice changes), the specs and snapshots it touches, `docs/decisions/2026-11-25-X-gs-followups-2.md`, and your slices.yaml entry and placement line. Not `web/src/lib/ui/lists/**`, `features/search/cards/**` or `features/settings/companions/**` (X-web-followups-1124, slot 1), and not `features/upload/**` or the settings shell (UP-2 #1106).

## Done
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and `npx playwright test` for the specs you touch, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` (never `playwright install`). No fixed sleeps. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green, and say whether any snapshots or size entries changed. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
