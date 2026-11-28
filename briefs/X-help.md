# X-help: help links per screen, /help, glossary popovers, what ≈ means, a "Something's wrong" page (runner brief, 2026-11-28)

Track U fill work (owner D1, 2026-11-28: Track U fills free slots; whatever is unfinished at the 1.0 cut moves to 1.1). Plan entry: runner/slices.yaml `X-help`.

## Scope (from the plan entry and the UI review)
- Sources: `docs/review/2026-11-13-ui-ux/` items U30, U31, D8, D10 and 04 §6d; `docs/architecture.md` §10.14; `docs/ui-glossary.md`; `docs/decisions/2026-11-20-X-ui-plan-r5.md` (the r5 UI plan, which this slice follows).
- A help link on each screen to its chapter of the user guide (`docs/user/`), a `/help` route listing them, glossary popovers for the terms in `docs/ui-glossary.md`, a plain explanation of what "≈" (an estimated count) means wherever it appears, and a "Something's wrong" page (what to check, where the logs are, how to report a problem) linked from help and from error states.
- Help text follows the user-docs skill (`.claude/skills/user-docs/SKILL.md`): plain, for the moderate user, safe choices recommended. Offline by default: help pages are bundled; links to the online guide are optional extras, never required.

## Owns
`web/src/lib/ui/help/**`, `web/src/routes/help/**`, this slice's e2e specs, the en and zh message files' new keys for help, `docs/decisions/*X-help*`. Wiring a help link into an existing screen needs one line in that screen's file: list each such file in step 0 and the runner grants them. No other screen changes.

## Rules
- Svelte 5, tokens only (no hex), Tailwind default spacing on the §10.2 ramp, Atkinson fonts; smallest UI: help routes and popovers are lazy chunks. Measure `npx size-limit` before and after; any first-load growth on an existing route needs a runner ruling; new lazy chunks get a limit of measured + at most 64 B.
- Accessibility: popovers are keyboard-reachable buttons with `aria-expanded`, Escape closes and focus returns; no nested interactive elements.
- No fixed sleeps in tests; condition waits with deadlines >= 10 s (vitest testTimeout is 20 s since #1184).
- Checks before pushing: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test` for the new specs (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`).

## Step 0 (send to the runner before building)
The list of screens and the guide chapter each links to; the glossary terms covered; the files outside owns that need a one-line hook; the expected new chunk sizes.
