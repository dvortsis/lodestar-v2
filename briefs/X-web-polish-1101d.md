# X-web-polish-1101d: keep the place after Block; headroom on /t and Detail sections
**Model:** Sonnet (pattern-following; the headroom part follows #304, #313 and #332). The runner reviews; an Opus reviewer only if the runner asks. Runner brief, 2026-11-01.

## Why
1. **Block loses your place.** After "Block this…" applies a rule, `RowActions.svelte` refetches the results on `ls-stale` (`results.reload()`). The list jumps back to the top and focus is lost, so a keyboard user who blocks row 40 lands on row 1.
2. **Two entries are at their limits** after #332:

| Entry | Size (gzip) | Limit |
|---|---|---|
| /t/[hash] route (own JS + CSS) | 20,969 B | 21.0 KB (31 B left) |
| Detail sections (on demand) | 37,298 B | 37.3 KB (2 B left) |

The next detail change can't land without a cut.

## Read first
- `web/src/lib/features/search/results/RowActions.svelte` (the `ls-stale` listener), the results model and `VirtualList` (how scroll and the focused row are kept), and `docs/decisions/2026-11-01-X-detail-polish.md` (#327, which added the refetch).
- `docs/decisions/2026-11-01-X-detail-headroom.md`, `docs/decisions/2026-11-01-X-search-headroom-2.md` (method, what worked; `runner_notes` → `rolldown-groups` in `state.json` on `runner-state`: no Rolldown chunk groups for app code).
- `web/.size-limit.mjs` (these entries and their comments), `web/scripts/first-load.mjs` (`modulesIn`).

## Do
1. **Keep the place on a Block refetch.**
   - After the reload, the list keeps its scroll position, and focus goes to the row that took the blocked row's place (the next row, or the previous if it was last). If nothing is left, focus goes to the results heading.
   - It's announced politely through the existing live region ("Blocked. 1 result removed." or similar; reuse an existing message if one fits), en **and** zh.
   - Only for the `ls-stale` reload; a new search still starts at the top.
   - vitest (fails with the fix removed; condition waits only) and a Playwright keyboard check in the existing search-results spec.
2. **Headroom:** ≥ 0.3 KB on /t and ≥ 0.3 KB on Detail sections.
   - In order: (a) move code not needed until interaction to an on-demand import, with no extra round-trip on first paint; (b) single forwarder roots so shared code stops landing in these chunks; (c) dead code or duplicate helpers.
   - Lower each limit you cut to measured + 10% of the cut, never below the 0.3 KB target. Never raise a limit; don't touch `vite.config.*`.
   - The drawer, Search results (40.61 KB), Send to client, Alert, Alerts and Results toolbar must not cross their limits.
3. **No visual change** apart from focus. Baselines must not change; if any do, stop and tell the runner.
4. **Decisions file:** `docs/decisions/<date>-X-web-polish-1101d.md` with before → after bytes for each entry that moved.

## Own
- `web/src/lib/features/search/results/**` (the reload, focus and announcement only)
- `web/src/lib/features/detail/**` and `web/src/routes/t/[hash]/**` (headroom only)
- these entries in `web/.size-limit.mjs`, the messages, specs and your decisions file

**Don't edit** `features/filters/**`, `features/shell/**`, `features/settings/**` or `vite.config.*`.

## Size
≲ 450 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; identical)
- `npx playwright test` for search-results*, detail*, keyboard* and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Check each commit's file list (`git show --stat`) for stray formatter passes before pushing.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the before/after table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
