# X-detail-headroom: headroom on the Detail drawer, /t/[hash] and Detail sections
**Model:** Sonnet, following the X-alert-search-headroom pattern (#304). An Opus reviewer follows. Runner brief, 2026-11-01. Owner-approved build order, item 4. It unblocks S-filters-2c ("Block this…").

## Why
These entries sit within bytes of their limits on main:

| Entry | Size | Limit |
|---|---|---|
| Detail drawer (own JS + CSS) | ~28.89 KB | 29 KB |
| /t/[hash] route (own JS + CSS) | ~22.53 KB | 22.6 KB |
| Detail sections (on demand) | ~34.44 KB | 34.5 KB |

S-filters-2c needs about 0.3 KB on the drawer and /t for a "Block this…" hook: one menu item plus one dynamic import through a forwarder. #308 also left the magnet copy/open shortcuts silent on a failed load, because a toast cost 130 B on Detail sections.

## Read first
- `docs/decisions/2026-10-31-X-alert-search-headroom.md`: the method, what worked, and why Rolldown chunk groups can't be used for app code (`runner_notes` → `rolldown-groups` in `state.json` on `runner-state`).
- `web/.size-limit.mjs` (how these entries are measured, around l.1100), `web/scripts/first-load.mjs` (`modulesIn`, the first-load guard).
- `web/src/lib/features/detail/**`, `web/src/routes/t/[hash]/**`, and the S-filters-2b decisions file (`docs/decisions/2026-10-31-S-filters-2b.md`, which records the measured hook cost and the flows-chunk regroup).

## Do
1. **Measure what each entry includes** (the size-limit JSON, the manifest, the chunk graph) and list the biggest contributors not needed on first view.
2. **Find real bytes, in this order:**
   - (a) code not needed until interaction moves to an on-demand import, with no extra round-trip on first paint;
   - (b) import restructuring (single forwarder roots) so shared code stops landing in these chunks;
   - (c) dead code or duplicate helpers.
3. **Targets:** ≥ 0.5 KB headroom on the drawer and /t, and ≥ 0.2 KB on Detail sections. Lower the limits if the headroom comes from a cut: new limit = measured + 10% of the cut, never below these targets. Never raise a limit.
4. **If Detail sections gains ≥ 0.2 KB,** give the magnet copy/open sites (`detail/commands.ts`, `search/results/RowActions.svelte`) the existing error toast on a failed chunk load (the #308 follow-up), as long as it fits.
5. **Don't change behaviour or visuals.** Baselines must not change; if any do, stop and tell the runner. No extra round-trip on first paint.
6. **Make it stick:** a short comment beside these entries in `.size-limit.mjs` naming what not to import into them. Add a first-load guard if it's cheap, like #304's.
7. **Decisions file:** `docs/decisions/<date>-X-detail-headroom.md`, with before/after bytes for each entry that changed.

## Own
- `web/src/lib/features/detail/**`, `web/src/routes/t/[hash]/**`
- the magnet sites in `search/results/RowActions.svelte` (only those lines)
- these entries in `web/.size-limit.mjs`
- your decisions file

**Don't edit** `features/filters/**`, `features/wizard/**`, `features/settings/**` or `vite.config.*`. Alert, Alerts, Search results and Results toolbar must not cross their limits.

## Size
≲ 500 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; the results must be identical)
- `npx playwright test` for detail*, search-results*, and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the before/after table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
