# X-discover-headroom: headroom on the Discover route
**Model:** Sonnet, following the X-alert-search-headroom (#304) and X-detail-headroom (#313) pattern. The runner reviews. Runner brief, 2026-11-01.

## Why
After #317 (X-illustrations) the "Discover route (own JS + CSS)" entry measures ~45.08 KB against a 45.2 KB limit (runner §2.26 raised it from 45 KB for the illustrations check). The next Discover feature has no room.

## Read first
- `docs/decisions/2026-10-31-X-alert-search-headroom.md`, `docs/decisions/2026-11-01-X-detail-headroom.md` (the method, forwarder modules, why Rolldown chunk groups can't be used for app code), `docs/decisions/2026-11-01-X-illustrations.md`.
- `web/.size-limit.mjs` (the Discover entry and its comment), `web/scripts/first-load.mjs` (`modulesIn`).
- `web/src/lib/features/discover/**`, `web/src/routes/discover/**` (or wherever the route lives).

## Do
1. **Measure** what the Discover entry includes (size-limit JSON, manifest, chunk graph) and list the biggest contributors not needed on first view.
2. **Find real bytes, in this order:** (a) code not needed until interaction moves to an on-demand import, with no extra round-trip on first paint; (b) import restructuring (single forwarder roots) so shared code stops landing in the route chunk; (c) dead code or duplicate helpers.
3. **Target:** ≥ 0.6 KB headroom under the current 45.2 KB limit. If the cut is larger, lower the limit: new limit = measured + 10% of the cut, never leaving less than 0.6 KB headroom. Never raise a limit.
4. **Don't change behaviour or visuals.** Baselines must not change; if any do, stop and tell the runner. No extra round-trip on first paint.
5. A short comment beside the Discover entry in `.size-limit.mjs` naming what not to import into it; a first-load guard if cheap.
6. **Decisions file:** `docs/decisions/2026-11-01-X-discover-headroom.md` with before/after bytes for each entry that moved.

## Own
`web/src/lib/features/discover/**`, the Discover route files, the Discover entry in `web/.size-limit.mjs`, your decisions file.
**Don't edit** `features/search/results/**`, `features/detail/**`, `features/filters/**`, `lib/shell/**`, `lib/ui/**` (other slices are near them), `web/e2e/**`, or `vite.config.*`. If the bytes are in a shared module you don't own, report it with numbers instead of editing it. Alert, Alerts, Search results, Results toolbar and CSS must not grow.

## Size
≲ 400 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; identical)
- `npx playwright test` for discover* and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the before/after table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
