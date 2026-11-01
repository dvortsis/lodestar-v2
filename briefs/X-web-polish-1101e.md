# X-web-polish-1101e: three small web follow-ups
**Model:** Sonnet (small, pattern-following fixes). The runner reviews. Runner brief, 2026-11-01. Items come from runner-state `followups` (#317, #322, #254).

## Do
1. **Illustrations off on a first visit (#317).** `web/src/lib/shell/StateScene.svelte` reads the `lodestar.appearance` cache (`features/appearance/prefs.ts` `CACHE`) to decide whether to show the scene. On a first visit there's no cache yet, so an account with "Show illustrations" off still gets the first scene once.
   - Make the scene wait for the real preference when the cache is missing (the appearance preference the app already loads; find it). Never show a scene and then remove it.
   - When the cache exists, nothing changes. No extra first-paint request; the scene may simply appear a moment later on a first visit.
   - Save-Data still hides it as now.
   - vitest: no cache + illustrations off → no scene ever rendered; no cache + on → scene once the preference arrives; cache present → as today.
2. **Discover `j`/`k` fail silently (#322).** `features/discover/DiscoverPage.svelte` (~l.113) imports `$lib/keys` and `./commands` after mount. If that import fails (offline, or a stale build after an update), `j`/`k` do nothing and nobody says so.
   - On a rejection, show the existing error toast with a neutral existing message if one fits; otherwise add a short one in en and zh.
   - Retry on the next `j`/`k` press only if that's simple; otherwise the toast is enough.
   - vitest with the import rejected (condition wait).
3. **Bulk manual-copy textarea stays open (#254).** In `features/search/bulk/BulkBar.svelte`, the manual-copy textarea (shown when the clipboard is refused) stays open after the selection changes, still holding the old selection's text. Close it, or refresh its text, when the selection changes. Pick whichever is clearer for a keyboard user and say why. vitest.

## Rules
- No visual change apart from these. Baselines must not change; if any do, stop and tell the runner.
- **Sizes:** Discover (43 KB limit), the shell first load, Search results (40.61 KB), Results toolbar and the bulk entries must not cross their limits. Each grows by at most +0.1 KB. Never raise a limit. Report before → after.
- Condition waits only; no fixed sleeps.
- Decisions file: `docs/decisions/<date>-X-web-polish-1101e.md` (short; one line per item plus the size table).

## Own
`web/src/lib/shell/StateScene.svelte` (and its test), `web/src/lib/features/discover/DiscoverPage.svelte` (the import site only) and its messages, `web/src/lib/features/search/bulk/**`, your decisions file.
**Don't edit** `features/search/assist/**` (another session owns it), `features/appearance/**` beyond reading an export, or `vite.config.*`. Don't run formatters over files you didn't change; check each commit's file list (`git show --stat`) before pushing.

## Size
≲ 300 changed lines.

## Done when
These pass in `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit` (twice; identical), and `npx playwright test` for discover*, search-bulk* (if present), smoke and core.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Don't background long jobs with `&`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
