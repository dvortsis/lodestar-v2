# X-settings-registry-slim: take per-screen titles out of the shared Settings chunk
**Model:** Opus (web performance; the chunk graph and the size gates). Runner brief, 2026-10-31. This must land before S-about, S-updates, S-wizard or S-filters adds a Settings registry entry.

## Why
Every Settings registry entry (`web/src/lib/features/settings/shell/screens.ts`, or wherever the registry lives) carries its title and purpose literals in en and zh inside the shared `screens` chunk. Every Settings first view loads that chunk.

The Logging entry (#289) cost about 81–190 B on every first view, and the Notifications first view needed a runner raise to 45.1 KB (§2.26, see the comment in `web/.size-limit.mjs` ~729). The Alert route and the Search results lazy chunk sit within a few bytes of their limits. Four more screens are queued, and each would add the same cost.

## Read first
- CLAUDE.md UI rules and `runner/standards.md`.
- `docs/decisions/2026-10-31-S-logs-2.md` (the registry cost, the `body.ts` forwarder), `-S-settings-2b.md` and `-M-security-settings.md` (how hand-built screens and lazy parts are split).
- The registry, its consumers (the nav, the overview, the search/palette entries if any), the message-map generator and `web/.size-limit.mjs` (the hand-built first-view walk and its stoppers).

## Do
1. **Move each screen's display strings out of the shared chunk.** Keep in the registry only what every first view needs: the id, group, icon key and route. Put the title and purpose behind the existing message-map mechanism, as keys resolved through the group's or the screen's own map, or another way that keeps them out of the shared chunk.
   - The nav still has to render all titles on first paint without a flash. Measure where the titles end up (the nav's own chunk, loaded once) and show that this costs less than today's per-entry literals in every first view.
   - zh keeps working.
2. **Lower the Notifications first-view limit back to 45 KB** and delete the runner-raise comment. Every Settings first view must keep ≥ 0.3 KB headroom.
3. **No other entry may grow.** Watch the Alert route (46 KB) and Search results (43.3 KB) especially. Never raise a limit. Report the before and after numbers for every Settings first view, Alert and Search results.
4. **Make adding a screen cheap from now on.** Add a short note in the registry file, plus a size-limit comment, saying what a new entry costs, measured.
5. **Tests:**
   - vitest for the registry and the nav (titles render in en and zh, order unchanged);
   - the Settings Playwright specs pass;
   - no baseline changes expected. If any appear, stop and tell the runner.
6. **Decisions file:** `docs/decisions/<date>-X-settings-registry-slim.md`, flagged under §2.19 (it reverses the 45.1 KB raise).

## Own
- The Settings registry and its direct consumers (nav, overview).
- The message-map generator only if needed, plus the regenerated maps.
- `web/.size-limit.mjs`: the Notifications line and comments, and new entries if a split creates one.
- Their tests and your decisions file.

**Don't touch** `features/settings/logging/**`. S-logs-2b is running there. If the logging screen's registry entry needs to change shape, change only that entry's line. Whichever PR merges second brings in main.

## Size
≲ 500 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the Settings specs

Chromium is at /opt/pw-browsers; never run `playwright install`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally: the seven-day usage warning is escalating. Push once per round of fixes.
