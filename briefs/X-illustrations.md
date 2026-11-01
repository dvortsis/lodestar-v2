# X-illustrations: make "Show illustrations" and Save-Data actually hide illustrations
**Model:** Sonnet (small, follows existing patterns). The runner reviews the PR. Runner brief, 2026-11-01. It closes an app gap found by S-e2e (#314) and is part of the M5 exit gate (§10.17, §10.19).

## Why
`docs/decisions/2026-11-01-S-e2e.md` ("App gaps"):
- `applyLook()` in `web/src/lib/features/appearance/prefs.ts` sets `data-illustrations="false"` on `<html>`, but nothing reads it. `StateScene.svelte` still renders the picture, so the Appearance toggle hides nothing.
- Nothing honours `Save-Data`. §10.19 says illustrations are off automatically when Save-Data is on.

## Read first
- `docs/architecture.md` §10.19 ("Illustration rules"), §10.17 (the gate), §10.10.8 (Appearance).
- `web/src/lib/shell/StateScene.svelte`, `StateScreen.svelte`, `web/src/lib/features/appearance/{prefs.ts,boot.ts,AppearanceScreen.svelte}`, the pre-paint script (search for `lodestar.appearance` in `web/src/app.html` or wherever it lives).
- `docs/decisions/2026-11-01-S-e2e.md` (the spec it describes) and `web/e2e/states.test.ts`.

## Do
1. **The toggle hides illustrations.** When the account's `illustrations` is false, no state illustration renders and **no image is fetched** (not just hidden with CSS after download: an `<img loading="lazy">` inside `display:none` isn't fetched, so a CSS rule keyed on `[data-illustrations='false']` is acceptable if you verify no request goes out; otherwise skip rendering the `<picture>`). It must hold from first paint (the pre-paint cache already sets the attribute) and follow a change made on the Appearance screen without a reload.
2. **Save-Data turns them off.** When `navigator.connection?.saveData` is true, illustrations are hidden whatever the stored preference, and the stored preference is **not** rewritten (the user's choice comes back when Save-Data is off). On the Appearance screen, the toggle shows that Save-Data is overriding it (one short hint line under the toggle, plain words, e.g. "Off while your browser's data saver is on."). Add that message to the appearance feature's en **and zh** files (H-zh's completeness test requires every en key to have a zh key).
3. **Low-resource mode** doesn't exist in the app yet; don't invent it. Name it as a follow-up in your decisions file.
4. **Don't change** anything else about the scenes (framing, variants, the < 600 px rule). Baselines must not change, except none should; if any image moves, stop and tell the runner.
5. **Tests:**
   - vitest: the toggle-off path and the Save-Data path each render no illustration (each fails with its guard removed); Save-Data doesn't rewrite the stored preference.
   - Playwright (`core` project): the spec S-e2e described. Set `illustrations: false` via the smoke mock's preferences, then assert `[data-testid="state-screen"] img` is absent and no request for a scene image was made; emulate Save-Data (e.g. `addInitScript` defining `navigator.connection.saveData = true`) and assert the same. Condition waits only (≥ 10 s), no fixed sleeps.
6. **Sizes:** the cost lands on first views that show scenes and on Appearance. Keep every entry under its limit, CSS must not grow by more than 0.05 KB, and never raise a limit. Report every size-limit entry that moves.
7. **Decisions file:** `docs/decisions/2026-11-01-X-illustrations.md`, short: what reads the flag, how Save-Data is detected, the low-resource follow-up, byte deltas.

## Own
`web/src/lib/shell/StateScene.svelte`, `StateScreen.svelte`, `web/src/lib/features/appearance/**` (incl. its messages), the pre-paint script's illustrations line if needed, the new e2e spec, your decisions file.
**Don't edit** `features/filters/**`, `features/search/results/**`, `features/detail/**` (another slice is in them), or `vite.config.*`.

## Size
≲ 300 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the new spec (3× clean), states*, appearance*, and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
