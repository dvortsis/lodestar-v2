# X-flows-door-2: route Appearance and the wizard through the filters flows door
**Model:** Opus (it touches the 18+ confirmation, a content-safety path; behaviour must not change). The runner reviews; `/security-review` is required. Runner brief, 2026-11-01. It closes the S-filters-2c (#316) review follow-up.

## Why
#316 made `web/src/lib/features/filters/flows-door.ts` the door into `features/filters/flows/**` and added a first-load guard in `.size-limit.mjs` for the Filters first view, the drawer and Search results. The reviewer found three importers still going around it:
- `features/appearance/AppearanceScreen.svelte` (around l.11: a static import of `flows/adult.svelte`; around l.160: `AdultConfirmDialog`);
- `features/wizard/PackStep.svelte` (around l.70: `flow.svelte`);
- the dev route that imports flows directly.

The guard doesn't cover the Appearance or wizard views, so a regroup like 2b's could land flows code in their first views unseen.

## Read first
- `docs/decisions/2026-11-01-S-filters-2c.md` (the door, the walk rule, the guard), `docs/decisions/2026-10-31-S-filters-2b.md` (the regroup it prevents).
- `web/src/lib/features/filters/flows-door.ts`, the three importers above, `web/.size-limit.mjs` (the flows guard and the Appearance / Wizard steps / Settings entries), `web/scripts/first-load.mjs`.
- `docs/architecture.md` §20.9 (the 18+ confirmation), §10.10.8 (Appearance).

## Do
1. **Route every importer through the door.** Whatever Appearance and the wizard need from `flows/**` comes through `flows-door.ts` (add the exports it needs there; the door may grow, the flows internals may not change). If Appearance needs something on its first view (e.g. the adult state it shows before any click), check whether that piece belongs in flows at all. If it's a small state module, it can move out to a shared non-flows module, but only if behaviour is identical; say what you chose.
2. **The dev route** goes through the door too, or is excluded from the guard with a comment if it's dev-only and never built into production.
3. **Extend the guard** so it fails if the Appearance first view or the wizard's first view carries `filters/flows/**` modules (the same rule as the drawer/Search/Filters guard). Show it fails when you put a direct import back.
4. **The 18+ confirmation must behave exactly as before:** turning "Show adult content" on asks for the confirmation, Cancel leaves it off and sends nothing, a refused save leaves the server state, and nothing adult shows until it's confirmed and saved. The existing Appearance and wizard tests must pass unchanged; add a vitest for any path whose loading changed (it waits on conditions, and it fails with its guard removed).
5. **Sizes:** the Appearance, Settings, Wizard steps and Filters entries must not grow beyond +0.05 KB each. Moving bytes from a first view to on demand is fine. Never raise a limit. Report every entry that moves.
6. **Decisions file:** `docs/decisions/2026-11-01-X-flows-door-2.md` with what moved, the guard, byte deltas.

## Own
`features/appearance/AppearanceScreen.svelte` (the import lines and what they need), `features/wizard/PackStep.svelte` (same), `features/filters/flows-door.ts`, the dev route's import, the flows guard and these entries in `web/.size-limit.mjs`, their tests, your decisions file.
**Don't edit** `features/filters/flows/**` internals, `features/detail/**`, `features/search/results/**` (S-send is there), `lib/shell/**`, `web/e2e/**` baselines, or `vite.config.*`.

## Size
≲ 300 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; identical)
- `npx playwright test` for appearance*, wizard*, filters*, settings* and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Run `/security-review`. Don't run prettier over anything you didn't change.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
