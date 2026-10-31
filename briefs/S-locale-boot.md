# S-locale-boot: apply the saved language, time zone and first day of week at start-up
**Model:** Sonnet: boot wiring per §10.12 and the i18n module. Runner brief, 2026-10-31. Follow-up from S-settings-2 (#259). No `/security-review`.

## Why
S-settings-2 built the Language & region screen (`preferences`: language, time zone, first day of week), but:
- `web/src/routes/+layout.ts` calls `initLocale(null)` ("Settings aren't wired yet"), so a saved language only applies after visiting the Language screen. Every reload falls back to `navigator.language`.
- `time_zone` and `first_day_of_week` aren't read anywhere outside Settings. Dates and times use the browser's zone, and any week-based UI starts on the browser's default.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md`, `docs/architecture.md` §10.12 (time zone and locale: what "display time zone" means, the defaults, and which screens show dates), §10.1 (first-load budget) and §10.10.8's Language & region row.
- `web/src/lib/i18n/README.md` and `locale.svelte.ts` (`initLocale`, `resolveLocale`), `web/src/routes/+layout.ts`, and `features/settings/screens2/LanguageForm.svelte` (how it applies a saved value now).
- How the app loads settings or preferences today (`lib/state/**`, the API client) and the existing date/time formatting helpers (search for `Intl.DateTimeFormat`, `toLocale*`, `formatDate`/`formatTime`).
- The `preferences` shape in `web/src/lib/api/schema.d.ts`.

## Do
1. **Language at boot:** apply the saved `preferences.language` before the first paint of translated text, or as close to it as the boot path allows without blocking first render on a slow request.
   - Fallbacks: saved → `navigator.language` → en.
   - A failed or slow preferences fetch must never block the app. Say what you chose (a cached value in `localStorage` as a per-viewer hint is acceptable if it's non-secret and the server stays authoritative) and why, in the decisions file.
   - "Automatic" (empty) keeps following the browser.
2. **Time zone:** one shared accessor for the display time zone (saved value, or the browser's when automatic). The shared date/time formatters use it, so every screen that shows dates follows the setting. Don't rewrite screens; route their formatting through the helper. List any screen that formats dates outside the helpers and fix it if small, otherwise list it in the decisions file.
3. **First day of week:** one shared accessor. Wire it into any existing week-based UI (date pickers, custom range calendars in Insights). If none exists, expose the accessor and say so.
4. **Live update:** changing these on the Language & region screen applies at once without a reload. LanguageForm already does this for language; make time zone and first day of week follow the same path.
5. **Sizes:** first load must not grow beyond a few hundred bytes. Never raise a limit; if you can't fit, stop and report.
6. **Tests:**
   - vitest: boot resolution order; a failed fetch falls back without throwing; the formatters use the saved zone (fixed `Date`, two zones); first day of week from the setting;
   - Playwright: a saved zh language shows zh after a reload; a saved time zone changes a visible timestamp; condition waits only, no fixed sleeps;
   - **no baseline changes expected.** The mock's default preferences must keep today's rendering. If screenshots change, stop and tell the runner.
7. **Decisions file:** `docs/decisions/<date>-S-locale-boot.md`.

## Own
`web/src/routes/+layout.ts`, `web/src/lib/i18n/**`, the shared date/time formatting helpers (wherever they live; name them in the PR), minimal call-site changes to route formatting through them, `features/settings/screens2/LanguageForm.svelte` (only for the live-apply path), the e2e mock's default preferences, `web/e2e/locale-boot.*`, and the decisions file. If this needs `lib/state/**` or `lib/api/**` changes, keep them minimal and justify each in the PR.

## Size
≲ 600 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the new spec 3× and the existing Settings and Insights specs. Chromium is at /opt/pw-browsers; never run `playwright install`. If the bundled headless shell doesn't match, use an untracked local config with `executablePath: '/opt/pw-browsers/chromium'` and never commit it.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally. Push once per round of fixes.
