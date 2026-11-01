# X-web-polish-1101c: privacy-logging banner, theme-pick flicker, snapshot chart unit
**Model:** Sonnet. The runner reviews. Runner brief, 2026-11-01. Review follow-ups from `state.json` → `followups` on `runner-state`.

## Items
1. **The privacy-logging banner** (#277 review; §10.10.7 "Privacy" row and §10.10.12). While a privacy-grade log data kind is on (query logging `insights.query_logging`, or any logging data kind that identifies torrents or people, if the Logging screen exposes them), a banner shows app-wide saying what is being recorded, with a link to turn it off (the Insights or Logging settings screen).
   - Find a signal the app **already loads** on first paint (a status/settings response, the SSE status, the logging state). Don't add a request to first paint. If no cheap signal exists, stop and tell the runner what the cheapest option costs in bytes and requests.
   - The banner is a working-surface notice (no illustration), dismissible only by turning the logging off, polite, keyboard-reachable. Plain words, en **and zh**.
   - Load its code on demand when the signal says logging is on, so first views grow by at most +0.05 KB.
2. **Theme-pick flicker** (#247). Quick successive theme picks on Appearance can flicker, because the boot/apply path re-applies a stale server value while a save is pending. Add a pending-save marker from AppearanceScreen that the boot/apply path honours (the existing `lookShown`/`themeShown` logic in `appearance/boot.ts`). A vitest that reproduces the flicker sequence and fails without the marker.
3. **Snapshot chart axis unit** (#258 nit). The Insights snapshot chart's axis unit is hard-coded "torrents". Make the unit a per-chart field (defaulting to torrents) so a second snapshot chart can set its own. No visual change.

## Read first
`docs/architecture.md` §10.10.7, §10.10.12, §10.18 (the pre-paint/boot theme path); `web/src/lib/features/appearance/{boot.ts,prefs.ts,AppearanceScreen.svelte}`; `web/src/lib/features/settings/screens2/InsightsScreen.svelte` (query logging) and the Logging screen; `web/src/lib/shell/**` (where app-wide banners live, e.g. the stale-tab banner from #307); the Insights snapshot chart component.

## Rules
- Sizes: every entry stays under its limit; first views +0.05 KB at most each; CSS must not grow; never raise a limit. Report entries that move.
- Baselines must not change (the banner only shows when logging is on; the smoke mock should have it off). If any change, stop and tell the runner.
- Condition waits only; no fixed sleeps. Don't run formatters over files you didn't change. Run long checks in the foreground.

## Own
`web/src/lib/shell/**` (the banner host only), a new on-demand banner module, `web/src/lib/features/appearance/**`, the Insights snapshot chart component, their messages and tests, your decisions file `docs/decisions/2026-11-01-X-web-polish-1101c.md`.
**Don't edit** `features/search/**`, `features/detail/**`, `features/filters/**`, `features/settings/updates/**`, `crates/**`, `vite.config.*`.

## Size
≲ 350 changed lines.

## Done when
These pass in `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit`, `npx playwright test` for appearance*, insights*, settings* and core.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, items done or skipped, and size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
