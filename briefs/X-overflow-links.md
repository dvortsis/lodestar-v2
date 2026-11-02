# X-overflow-links: the overflow menu links only to screens that exist, plus Logs and the Syntax guide
**Model:** Sonnet (UI, following an existing pattern). The runner reviews on Opus. Runner brief, 2026-11-02.

## Why
`web/src/lib/shell/nav.ts` declares `MoreItem` ids `logs` and `syntax`, but `MORE` doesn't offer them. Check whether every entry it does offer (`/settings/import`, `/settings/backup`, `/settings/about`) is a registered Settings screen. Any that isn't must not be linked: §10.3 says only existing routes are offered.

## Read first
- `docs/architecture.md` §10.3 (navigation, the overflow menu and the command palette).
- `web/src/lib/shell/nav.ts` and `OverflowMenu.svelte`, and how Settings screens register (`web/src/lib/features/settings/screens*`, `routes/settings/[section]`).
- Where the Logs screen and the query-syntax guide live today: grep for the `logs` section and "syntax".
- CLAUDE.md "UI rules", `docs/design-tokens.md` and `docs/ui-glossary.md` (labels).

## Do
1. **Fix `MORE` so it lists only existing screens.**
   - Derive each item from the screen registry if that's cheap; otherwise add a unit test that fails when an item's href has no registered screen.
   - Add **Logs** (the logging screen) and **Syntax guide** (wherever the query-syntax help lives) if they exist. If the syntax guide has no route, leave it out and say so in the PR.
2. **Labels:** add en and zh messages to `web/src/lib/features/shell/messages/*.json`, using the glossary terms.
3. **Tests:**
   - a vitest test for the registry check;
   - extend the existing shell or overflow e2e spec, if one exists, to click each item and land on a real screen (no "not available" state).
4. **Decisions file:** only if you make a call beyond §10.3. Name it `docs/decisions/2026-11-02-X-overflow-links.md`.

## Own
`web/src/lib/shell/nav.ts`, `web/src/lib/shell/OverflowMenu.svelte`, `web/src/lib/features/shell/messages/*`, their tests, and the regenerated message catalogue if the build commits one.

**Don't edit:** Settings screens or `web/src/routes/settings/**` (X-settings-overview owns those).

## Done when
In `web/`, these all pass: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for the touched spec. Use `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`, and never run `playwright install`. Size limits must not rise.

Branch `slice/X-overflow-links`. Open the PR titled "X-overflow-links: overflow menu offers only existing screens, plus Logs". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
