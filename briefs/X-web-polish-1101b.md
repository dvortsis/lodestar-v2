# X-web-polish-1101b: small a11y and correctness follow-ups
**Model:** Sonnet (small, well-specified fixes). The runner reviews. Runner brief, 2026-11-01. All items are review follow-ups logged in `state.json` → `followups` on `runner-state`.

## Items (do each; if one turns out bigger than stated or wrong, skip it and say why in the PR)
1. **Insights Movement annotations** (#265 review note 5). `web/src/lib/features/insights/overview/Movement.svelte` (and `ImportMarkers.svelte` if that's where the tabindex lives): only import markers are keyboard-reachable. Make restart and pause notes reachable the same way (same tabindex/role pattern and accessible name). A vitest.
2. **TimeZoneWidget alert per keystroke** (#259). The time-zone field (find it via `TimeZoneWidget` / `screens2/LanguageForm.svelte`) fires `role="alert"` on every invalid keystroke while typing. Show the invalid-zone error on blur (or after typing stops, debounced ≥ 500 ms via a condition, not a sleep in tests), so screen readers don't announce on each key. Keep the error visible once shown until the value is valid. A vitest.
3. **live.svelte.ts double again()** (#275 optional a). `web/src/lib/features/search/live/live.svelte.ts`: a tab hidden ≥ 35 s fires both the visibility path and the status path, so `again()` runs twice (an extra POST+DELETE). Coalesce a second `again()` within 1–2 s into the first. A vitest with fake timers.
4. **live.svelte.ts effect tracking** (#275 optional b): the status `$effect` calls `again()`, which reads `deps.shown()`, so the effect tracks search state. Wrap that call in `untrack`. A vitest that would fail if the effect re-ran on a search-state change.
5. **Flows guard gap** (#325): the first-load guard in `web/.size-limit.mjs` doesn't catch a direct `import()` of `features/filters/flows-root.ts` or `flows-adult.ts` from anywhere but `flows-door.ts`. Add a check (in the size config or a small vitest that scans `web/src` for importers) that fails if any module other than `flows-door.ts` imports either root. Show it fails with a planted import.

## Read first
CLAUDE.md, the files above and their tests, `docs/decisions/2026-11-01-X-flows-door-2.md` (item 5), `docs/architecture.md` §10.16/§10.17 for keyboard/a11y rules.

## Rules
- No behaviour change beyond each item. Sizes: every entry stays under its limit; no first view may grow by more than 0.05 KB; CSS must not grow; never raise a limit. Report entries that move.
- Condition waits only; no fixed sleeps. Don't run formatters over files you didn't change.

## Own
The files named in items 1–5 and their tests, the guard lines in `web/.size-limit.mjs` (item 5 only), your decisions file only if an item changes a recorded decision.
**Don't edit** `features/detail/**`, `features/search/results/**` (X-detail-polish), `features/settings/updates/**` (S-updates-4), `features/settings/security/**` (X-flake-security-test), `vite.config.*`.

## Size
≲ 300 changed lines.

## Done when
These pass in `web/`: `npm run check`, `npx vitest run`, `npm run build` then `npx size-limit`, and `npx playwright test` for insights*, settings* and core.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, items done or skipped, and size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
