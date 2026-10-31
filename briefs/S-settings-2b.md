# S-settings-2b: the Settings screens Integrations, Notifications, Trackers, Insights & digest
**Model:** Sonnet: UI per §10.10.8, on the Settings form engine with per-section messages (#264). Runner brief, 2026-10-31. The four screens S-settings-2 (#259) moved out until the server had them. B-settings-sections part a (#263) and part b (test kinds and clear usage) have merged. `/security-review` is required on the diff: these screens handle secrets, send unsaved values to `POST /settings/test`, and run a destructive action.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md` and `docs/owner-decisions.md`.
- **`docs/decisions/2026-10-31-B-settings-sections.md`, "What S-settings-2b's screens bind to": the field-level spec**, plus part b's decisions file (test kinds, the result shape, the clear-usage confirmation).
- `docs/architecture.md` §10.10.1–10.10.5, §10.10.7 (risky changes: the Privacy dialog on `query_logging`, Destructive for Clear usage data and Remove a secret), **§10.10.8's rows for these four screens**, §10.10.11 (accessibility, performance, tests), and §10.9.7 (leave a mount point for S-insights-3's digest preview; don't build it).
- `docs/decisions/2026-10-31-S-settings-2.md` and `-M-settings-engine-2.md`, and the engine README (`web/src/lib/features/settings/form/**`). Read how sections register (`screen.ts`, the registry glob) and how the Language screen is built (`screens2/**`).
- The API shapes in `web/src/lib/api/schema.d.ts`.

## Do
1. **Four screens** in `web/src/lib/features/settings/screens2/**`, registered through the screen registry. Generate from the schema wherever the bind-to list says "generated"; hand-build only:
   - **Integrations:** the download-client card list (add, remove, reorder if the schema allows), `default_client` select over the cards' ids, and Torznab fields from the schema. Each card and Torznab gets **Test connection** with the unsaved values through `POST /settings/test`, with the result inline as plain text.
   - **Notifications:** groups ntfy, Discord, email, webhook and routing, generated. Each channel gets **Send test**.
   - **Trackers:** scrape and cadence, generated. Health per tracker only if an endpoint exposes it; otherwise say so in the decisions file and leave a slot.
   - **Insights & digest:** digest and usage groups, generated. The Privacy dialog on `query_logging` (§10.10.7), a mount point for the digest preview, and **Clear usage data** as a destructive action with the confirmation part b requires.
2. **Secrets:**
   - Use the engine's secret field: never display or re-send a saved secret unless changed, and show its `secrets_set` state.
   - **Remove a secret** is its own confirmed `PATCH` from the field's menu (`null` for a field, `""` for a client password), not part of Save.
   - Nothing secret goes in localStorage, URLs or console output. Test results render as plain text only.
3. **States:** loading, error, env-locked or read-only, conflicts and leave-guard, all through the engine. Don't re-implement them. Each section's messages load on demand (#264's per-section maps). A failed map shows the existing banner.
4. **Engine polish (carried from #264's review),** only where these screens need it: reset `editing` when Automatic turns on (`SettingField.svelte:150-154`). The other #264 polish items are optional.
5. **Sizes:** `/settings/[section]` stays within its owner-set 45 KB budget, counting the section's own map plus the group map (#264's honest gates). Load hand-built parts on demand. Add entries at measured + 10%; never raise an existing limit. If you can't fit, stop and report the numbers.
6. **i18n:** the keys the bind-to list names, in en and zh, in the feature's messages. Regenerate the message maps with the repo's generator.
7. **Tests:**
   - vitest: Test connection and Send test with unsaved values (success, failure, server error shown as text); secrets not re-sent when unchanged; Remove a secret as a separate confirmed PATCH; Clear usage data confirmation; the Privacy dialog;
   - Playwright: each screen renders and saves against the mock, keyboard-only, with axe;
   - condition waits only, no fixed sleeps;
   - **Settings baselines may need regenerating** for the new nav entries. Use the `web/e2e/.update-snapshots` marker as S-appearance did, and check that only Settings images change.
8. **Decisions file:** `docs/decisions/<date>-S-settings-2b.md`.

## Own
`web/src/lib/features/settings/screens2/**`, these screens' message keys and regenerated maps, `form/SettingField.svelte` (item 4 only), the Settings mock fixtures, `web/e2e/settings-screens2b.*`, the size-limit entries, and the decisions file. **Don't edit** the Settings shell, other engine files, `lib/state/**`, `lib/shell/**`, `lib/ui/**` or `lib/api/**`. If the engine lacks something, stop and tell the runner.

## Size
≲ 1,500 changed lines. If it grows past that, split it: **2b-1** Integrations and Notifications; **2b-2** Trackers and Insights & digest.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the Settings specs, 3× for anything new. Chromium is at /opt/pw-browsers; never run `playwright install`. If the bundled headless shell doesn't match, use an untracked local config with `executablePath: '/opt/pw-browsers/chromium'` and never commit it.

Also run `/security-review` on your diff.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally. Push once per round of fixes.
