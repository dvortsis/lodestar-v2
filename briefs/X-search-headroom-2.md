# X-search-headroom-2: headroom on Search results, plus two detail follow-ups
**Model:** Sonnet, following the X-alert-search-headroom (#304), X-detail-headroom (#313) and X-discover-headroom (#322) pattern. The runner reviews. Runner brief, 2026-11-01.

## Why
After #321 (S-send) and #327 (X-detail-polish), "Search results" measures about 43,207 B against its 43,300 B limit (~93 B left). The next search feature has no room. Two small detail follow-ups from #327 ride along.

## Read first
- `docs/decisions/2026-10-31-X-alert-search-headroom.md`, `2026-11-01-X-detail-headroom.md`, `2026-11-01-X-discover-headroom.md` (methods; why Rolldown chunk groups can't be used for app code; the `byChunk` walk option from #322).
- `web/.size-limit.mjs` (the Search results entry and its comments), `web/scripts/first-load.mjs`.
- `web/src/lib/features/search/results/**`, `web/src/lib/features/detail/**`, `docs/decisions/2026-11-01-X-detail-polish.md` (its follow-ups).

## Do
1. **Measure** what "Search results" includes and list the biggest contributors not needed on first view.
2. **Find real bytes** in this order: (a) code not needed until interaction moves on demand, with no extra round-trip on first paint; (b) single forwarder roots so shared code stops landing here; (c) dead code or duplicates.
3. **Target:** ≥ 0.6 KB headroom under the current 43.3 KB limit. If the cut is larger, lower the limit to measured + 10% of the cut, never leaving less than 0.6 KB. Never raise a limit.
4. **`s` on /t before the torrent loads** (#327 follow-up): gate the `s` key until the torrent is loaded. The real place is `features/detail/DetailHostBody.svelte` (or wherever /t knows it's loaded). A vitest.
5. **A neutral error string** for a failed flows-door load in the drawer (#327 reused the magnet copy's string). Add a short plain message, en **and zh**, on the on-demand side only.
6. **Don't change behaviour or visuals** beyond 4–5. Baselines must not change; if any do, stop and tell the runner. Safety tests in `filters/block-entry.test.ts` stay as strict as they are.
7. A comment beside the Search results entry naming what not to import into it. **Decisions file:** `docs/decisions/2026-11-01-X-search-headroom-2.md` with before/after bytes for every entry that moved.

## Own
`web/src/lib/features/search/results/**`, `web/src/lib/features/detail/**` (items 4–5 and any byte moves), the Search results entry in `web/.size-limit.mjs`, their messages and tests, your decisions file.
**Don't edit** `features/filters/flows/**`, `features/filters/flows-door.ts`, `features/settings/**`, `lib/shell/**`, `web/e2e/**` baselines, or `vite.config.*`. If the bytes are in a shared module you don't own, report it with numbers instead of editing it. Alert, Alerts, Results toolbar, Discover, the drawer, /t and CSS must not grow.

## Size
≲ 400 changed lines.

## Done when
These pass in `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit` (twice; identical), and `npx playwright test` for search-results*, detail*, keyboard-only and core.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Don't run formatters over files you didn't change.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the before/after table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
