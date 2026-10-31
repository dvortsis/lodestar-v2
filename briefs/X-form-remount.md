# X-form-remount: keep Settings form children mounted through a resync
**Model:** Sonnet (Svelte engine UI, pattern-following). An Opus reviewer follows. Runner brief, 2026-10-31. This is the follow-up from the #289 review.

## Why
`SettingsForm` (in `web/src/lib/features/settings/form/**`) unmounts its children while `form.status === 'loading'`, which happens on a resync after a save or a server change. Every remount:
- resets local UI state in hand-built widgets: the Logging event list's search text and "Show changed only", plus open/closed state (#289 worked around that one with module-level state);
- drops keyboard focus to `<body>` after each toggle.

That's an accessibility defect (WCAG 2.4.3 focus order). It also makes every hand-built screen work around it.

## Read first
- CLAUDE.md UI rules.
- `runner/standards.md`.
- The engine README and `form/**`: how status changes, how the body renders, and the leave guard.
- `docs/decisions/2026-10-31-S-logs-2.md` (the module-level workaround).
- `features/settings/logging/EventList.svelte`, to see the workaround.

## Do
1. **Keep the children mounted while a resync loads.**
   - The first load still shows the skeleton.
   - A later load (resync) keeps the current body mounted and marks it busy: `aria-busy="true"` on the form region, inputs not interactive if the engine already disables them during a save, and no layout jump.
   - A resync that fails keeps the body and shows the engine's existing error state.
2. **Focus:** after a toggle or save, focus stays on the control the user used. Add a Playwright keyboard test for this on two screens:
   - one generated screen;
   - the Logging event list: toggle a row with Space, wait for the resync, and assert `document.activeElement` is still that switch and the list search text is unchanged.
3. **Remove the #289 workaround only if it's now redundant** (`opened` at module level in `EventList.svelte`): a one-line change back to component state. Keep it if removing it adds risk, and say why.
4. **Don't change behaviour beyond this.**
   - Engine semantics stay the same: staged changes, risky changes, Undo, the leave guard.
   - No size growth beyond +0.2 KB on any first view. Never raise a limit.
   - No baseline changes expected. If any appear, stop and tell the runner.
5. **Tests and records:**
   - vitest for the engine: a resync keeps the children mounted, the first load shows the skeleton, and a failed resync keeps the body.
   - Playwright as in item 2, keyboard-only, with axe.
   - Use condition waits only.
   - Write the decisions file `docs/decisions/<date>-X-form-remount.md`.

## Own
`web/src/lib/features/settings/form/**` (the status/render path only) and its tests, `features/settings/logging/EventList.svelte` (item 3 only, one line), a new `web/e2e/settings-form-focus.keyboard.test.ts`, and your decisions file.

**Don't touch:**
- the Settings registry, nav or `shell/**` (X-settings-registry-slim is running there);
- `features/settings/logging/**` beyond item 3 (S-logs-2b is running there). If item 3 conflicts, skip it and say so.

## Size
≲ 300 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test` for the Settings specs (3× for the new spec)

Chromium is at /opt/pw-browsers; never run `playwright install`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
