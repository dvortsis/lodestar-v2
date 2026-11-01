# S-filters-2c: "Block this…" in a result's menu and the detail drawer
**Model:** Opus. It's content-safety UI with a hard chunk-graph problem. An **Opus reviewer runs `/security-review`, and the safety rules below are blocking.** Runner brief, 2026-11-01. It completes S-filters (§20.9).

## Why
2b measured the hook and moved it here, because of size and a chunk regroup:
- **Budget:** the hook (one menu item or button, one state, one dynamic import through a forwarder) costs about 0.3 KB each on Search results, the Detail drawer and /t. Since then, #304 and #313 made room: Search results 42.78/43.3 KB, drawer 26.69/27.2 KB, /t 20.33/20.8 KB.
- **The real problem:** once Search and the drawer can reach the flows' dialogs, Rolldown regrouped the shared flow and `lib/ui` overlay code into a chunk that the **Settings → Filters screen imports statically**. Its first view went from 32.99 to 59.48 KB, and "Filters dialogs (on demand)" fell from 30.4 to 9.2 KB. Rolldown manual chunk groups can't be used for app code (`runner_notes` → `rolldown-groups`; #304).

## Read first
- `docs/decisions/2026-10-31-S-filters-2b.md` (the measurements), `-S-filters.md` (2a: how the screen loads the flows lazily), `-X-alert-search-headroom.md` and `docs/decisions/2026-11-01-X-detail-headroom.md` (forwarder and first-load techniques), and the runner notes in `state.json` on `runner-state`.
- `docs/architecture.md` §20.9 ("Block this…"), §20.10 and §20.11 (change flow, preview, warnings), §20.3 (locked floor).
- `web/src/lib/features/filters/flows/**` (use it; don't re-implement it), `web/src/lib/features/search/results/RowActions.svelte`, `web/src/lib/features/detail/DetailActions.svelte`, `web/.size-limit.mjs`, `web/scripts/first-load.mjs`.

## Do
1. **"Block this…"** in a result row's menu and in the detail drawer (and /t, if it shares DetailActions). It opens the flows' change dialog with a rule pre-filled from the torrent (§20.9 says which fields: hash, title pattern, uploader or tracker as the spec allows). It shows the impact preview and applies through the flow. It only ever **adds a blocking rule**.
2. **Solve the regroup, measured:** make one forwarder module the **only** door into `filters/flows/**` for every consumer, the Filters screen included. Then the flows and overlay code have a single dynamic-import root and stay in one on-demand chunk.
   - Check with the chunk graph (`modulesIn`) that the Filters first view doesn't carry flows or overlay modules.
   - If `lib/ui` overlay code is shared with first views, find which import pulls it in, and restructure that import (e.g. the forwarder imports the overlay host dynamically too).
   - Add a first-load guard in `.size-limit.mjs` that fails if the Filters first view, the drawer or Search results carries `filters/flows/**` modules.
3. **Safety rules (blocking):**
   - Block only adds a rule; there's no path to remove, weaken or except anything from this entry point.
   - Cancel sends nothing; a failed or refused apply leaves the server state, with the flow's error.
   - The pre-filled rule never contains more than the user sees (no hidden fields), and held or blocked names follow §20 (nothing rendered beyond what's allowed).
   - Nothing adult is shown unless 18+ is on.
4. **Sizes:**
   - Search results, the drawer and /t: at most +0.35 KB each, and each keeps ≥ 0.15 KB headroom.
   - The Filters first view must not grow (≤ +0.05 KB); "Filters dialogs (on demand)" may move but stays under its limit.
   - Alert, Alerts and Results toolbar must not cross their limits; CSS must not grow.
   - Never raise a limit. Don't touch `vite.config.*`.
5. **Tests:**
   - vitest per safety rule (each fails with its guard removed: wait on conditions, not right after the click);
   - Playwright: the keyboard-only path to Block from a result and from the drawer, with axe;
   - condition waits only.
6. **Decisions file:** `docs/decisions/<date>-S-filters-2c.md`, with before/after bytes for every entry above and how the regroup was solved.

## Own
- `web/src/lib/features/filters/screens/**` (the screen's door into the flows only)
- a new forwarder module (e.g. `features/filters/flows-door.ts`)
- the hook lines in `RowActions.svelte` and `DetailActions.svelte` (and /t if needed)
- the guard and size entries in `.size-limit.mjs`
- its messages, e2e specs and your decisions file

**Don't edit** `filters/flows/**` internals; if one export is missing, tell the runner.

## Size
≲ 600 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; the results must be identical)
- `npx playwright test` for filters*, search-results*, detail*, and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
