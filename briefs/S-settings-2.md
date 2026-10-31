# S-settings-2: the Settings screens Integrations, Notifications, Trackers, Insights & digest, Language & region
**Model:** Sonnet: UI per §10.10.8, on the merged Settings shell and form engine (S-settings-reconcile, #217). Runner brief, 2026-10-31. M5 screen backlog. Its dependencies have merged: the Settings shell and engine (reconcile), B9 (alerts and notifications), and B11c (digest). This brief needs `/security-review` on the diff for one reason: Integrations and Notifications handle **secrets** (client passwords, API keys, webhook URLs), and "Test connection" sends unsaved values to the server.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md` and `docs/owner-decisions.md`.
- `docs/architecture.md`:
  - §10.10.1–10.10.5 (principles, the field, saving, validation, conflicts);
  - §10.10.7 (risky changes);
  - **§10.10.8 Screens, the rows for these five screens (the spec);**
  - §10.10.11 (accessibility, performance, tests);
  - §10.12 (time zone);
  - §10.9.7 (the digest; its preview is S-insights-3's, so leave a mount point and don't build it).
- The S-settings-reconcile decisions file and the Settings form engine's README (`web/src/lib/features/settings/form/**`). The 45 KB per-route budget for `/settings/[section]` is an **owner decision**.
- How screens register: `web/src/lib/features/settings/screen.ts` and the screen registry's `import.meta.glob` (see `runner/slices.yaml` S-settings-2's notes). The Appearance and Keyboard screens are working examples of hand-built screens.
- The API: `integrations`, `trackers`, `insights`, the digest and `preferences`, plus `POST /settings/test`. Take shapes from `web/src/lib/api/schema.d.ts`. **If an endpoint you need is missing, stop and tell the runner.**

## Do
1. **Five screens** in `web/src/lib/features/settings/screens2/**`, registered through the screen registry. Prefer generated groups from the schema, and hand-build only what the spec requires:
   - **Integrations:** download clients and Torznab/*arr. Each has **Test connection**, which runs on the unsaved values through `POST /settings/test` and shows the result inline ("Connected to qBittorrent 5.0.2" / "Refused: wrong password").
   - **Notifications:** channels (ntfy, Discord, email, webhook), each with **Send test**, plus which events go where.
   - **Trackers:** the tracker list, scrape on/off and cadence, and health per tracker.
   - **Insights & digest:** digest schedule and channels, with a mount point for S-insights-3's preview; query logging and retention; "Clear usage data" as a destructive action with confirmation, per §10.10.7.
   - **Language & region:** language, time zone and first day of week (`preferences`).
2. **Secrets:**
   - Use the form engine's secret field. A saved secret is never displayed or re-sent unless the user changes it.
   - The test endpoint gets the value the screen holds; never log it.
   - Nothing secret goes in `localStorage`, URLs or console output.
   - Test results show server text as plain text only.
3. **States:** loading, error, env-locked or read-only values shown locked, conflicts, and leave-guard, all through the engine. Don't re-implement any of them.
4. **Sizes:** `/settings/[section]` stays within its owner-set 45 KB budget. Load hand-built parts on demand where needed. Add entries at measured + 10%, and never raise an existing limit. If you can't fit, stop and report the numbers.
5. **i18n:** `settings_*` keys for these screens in the feature's `messages/{en,zh}.json`.
6. **Tests:**
   - vitest: Test connection and Send test with unsaved values, success, failure and errors; secrets not re-sent when unchanged; Clear usage data confirmation;
   - Playwright: each screen renders and saves against the mock, keyboard-only, with axe;
   - condition waits only, no fixed sleeps;
   - **existing Settings baselines may need regenerating, because new screens appear in the navigation.** If so, use the `web/e2e/.update-snapshots` marker the way S-appearance did, and check that only Settings images change.
7. **Decisions file:** `docs/decisions/<date>-S-settings-2.md`.

## Own
`web/src/lib/features/settings/screens2/**`, these screens' message keys, `web/e2e/settings-screens2.*`, the size-limit entries, and the decisions file.

**Don't edit:** the Settings shell, the form engine, `lib/state/**`, `lib/shell/**`, `lib/ui/**` or `lib/api/**`. If the engine lacks something, stop and tell the runner.

## Size
≲ 1,500 changed lines. If it grows past that, split it:
- **2a:** Integrations and Notifications;
- **2b:** Trackers, Insights & digest, Language & region.

## Done when
These pass in `web/`:
- `npm run check`
- **`npm run e2e:typecheck`**
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the Settings specs, 3× for anything new (Chromium is at /opt/pw-browsers; never run `playwright install`)

Also run `/security-review` on your diff.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
