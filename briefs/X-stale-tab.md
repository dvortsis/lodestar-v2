# X-stale-tab: app-wide "a newer Lodestar is running" check
**Model:** Sonnet (small shell wiring). An Opus reviewer follows. Runner brief, 2026-11-01. Follow-up from S-about (#299).

## Why
After an upgrade, an open tab keeps running the old UI against the new server. S-about's check (`features/settings/about/logic.ts`, `UI_BUILD = __SVELTEKIT_APP_VERSION__` vs `/system` `ui_build_id`) only runs on Settings → About, and that chunk may not even exist on the new server. The shell needs one cheap check on every page that offers a reload.

## Read first
- CLAUDE.md UI rules, `runner/standards.md`, `docs/ui-glossary.md`.
- `docs/architecture.md` §10.10.13 (stale-tab wording), §10.10.6 (restart overlay), and the shell sections (status pill, banners).
- `web/svelte.config.js` and `web/scripts/build-version.mjs` (X-build-stable-name: the version name is the package version plus the 7-char SHA).
- `web/src/lib/shell/**`, especially `status.svelte.ts` / `status.ts` (what the shell already polls) and `Chrome.svelte` / `Shell.svelte`.
- The runner notes in `state.json` on `runner-state` (keys `runner_notes`, including the `$app/environment` note: importing it renames the runtime chunk's exports and grows every route; use `__SVELTEKIT_APP_VERSION__` directly).
- `docs/decisions/2026-11-01-X-alert-search-headroom.md` (or the dated file from #304): what must stay out of first loads.

## Do
1. **Pick the cheapest signal, measured:**
   - (a) Piggyback on a request the shell already makes, if its response carries the server's UI build id (or can be compared to `__SVELTEKIT_APP_VERSION__`). No new request.
   - (b) Otherwise SvelteKit's built-in version check (`_app/version.json`, via `kit.version.pollInterval` or `updated.check()` on `visibilitychange`), if it costs fewer bytes than (c).
   - (c) Otherwise one `GET /system` on `visibilitychange` (tab shown again), at most once per 5 minutes, never stacked.

   Report bytes on each first load for the option you chose (and why not the others, if you measured them).
2. **Banner:** when the builds differ, a quiet non-blocking banner, "A newer version of Lodestar is running. Reload to update.", with a Reload button. Use the existing `lib/ui` Banner and glossary wording; no new CSS (the CSS budget has about 30 B left). It doesn't steal focus; it's announced politely.
   - Never reload automatically: the user may have unsaved edits. If a Settings draft is dirty, the banner says so ("Save or discard your changes first") and Reload still asks via the existing leave guard.
   - A failed or missing check shows nothing (no false banner). Dev builds (`npm run dev`) never show it.
3. **S-about:** make About use the same check (one source of truth) if that's cheap; otherwise leave it and say so. Don't change About's visible behaviour.
4. **Sizes:**
   - The added code on every first load ≤ 0.15 KB gzip; anything bigger loads on demand only when the builds differ (the banner itself can be lazy).
   - Alert (≈0.6 KB headroom), Alerts, Search results, Results toolbar and the Search first load must stay under their limits; CSS must not grow. Never raise a limit; don't touch `vite.config.*`.
5. **Tests:**
   - vitest: same build → no banner; different build → banner; failed check → no banner; no request stacking; the 5-minute throttle (fake timers); dirty draft wording.
   - Playwright: the mock serves a different build id → the banner shows with axe clean, and Reload is keyboard-reachable. Condition waits only.
   - No baseline changes expected (the mock serves the same build by default). If any change, stop and tell the runner.
6. **Decisions file:** `docs/decisions/<date>-X-stale-tab.md`, with the option chosen and the bytes.

## Own
- `web/src/lib/shell/**` (the check and the banner wiring), a new `web/src/lib/shell/stale*` module if you want one
- the mock's build-id override for the e2e test
- `web/e2e/stale-tab*`, your decisions file, any glossary/message keys it needs (`shell_*`)
- the S-about call site, only if item 3 is cheap

**Don't edit** `features/wizard/**` (S-wizard is running), `features/settings/updates/**` (S-updates is running), `features/settings/form/**`, `lib/api/**` or `vite.config.*`.

## Size
≲ 400 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test e2e/stale-tab* e2e/shell*` (3× for the new spec)

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
