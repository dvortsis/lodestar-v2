# X-settings-trim: win back the Settings chunk's size headroom under the §10.1 cap
**Model:** Sonnet (a well-specified web size pass). A fresh Opus reviewer follows. Runner brief, 2026-11-05T12:35Z.

## Why
Recent slices (SRC-15a, SRC-17, LEGAL-1's About → Legal) raised the shared Settings chunk's `size-limit` entry to about 45.07–45.14 kB, past the §10.1 45 kB cap. The runner accepted those raises under §2.26 as temporary. This slice wins the bytes back so the entry can return to ≤ 45.0 kB, **with no change in behaviour or appearance**.

## Read first
`docs/architecture.md` §10.1 (budgets) and §10.2, `docs/design-tokens.md`, `web/.size-limit.mjs` (the Settings, Notifications and Detail entries and their comments), `web/src/lib/features/settings/**`, and the decisions files of SRC-15a, SRC-17 and LEGAL-1 (`docs/decisions/2026-11-0[45]-*.md`), which name the raises.

## Do (branch `slice/X-settings-trim` from `origin/main`)
1. Run `npx size-limit` and the build's chunk report to find what is in the Settings chunk that needn't be: panes that could be lazy (About → Legal, rarely opened panes), message keys pulled in by a shared import, duplicated helpers, and large static tables.
2. Make pure moves that change no behaviour: lazy-load rarely used panes behind the existing `{#await}` pattern with a `{:catch}` line, and split imports. Don't change any visible text, layout or keyboard flow.
3. Bring the Settings entry back to ≤ 45.0 kB. If Notifications or Detail, which share chunks, come down too, lower their limits to what you measured plus about 2%. Never raise one to make room for another. The first view must not grow.
4. Tests: the existing vitest and Playwright settings specs pass unchanged. Add vitest for any new lazy boundary (the loading line, and the catch line on a failed import).
5. Record the before and after bytes of each entry in `docs/decisions/2026-11-05-X-settings-trim.md`. Add your `runner/slices.yaml` entry and placement, then run `cd tools/status-connector && npm test`.

## Own
`web/src/lib/features/settings/**` (moves only), `web/.size-limit.mjs` (the Settings, Notifications and Detail entries only), the decisions file and your slices.yaml entry. **Don't touch** `web/src/lib/features/filters/**`, which RP-6 (#487) owns, or `web/src/lib/features/sources/**` (SRC-13c).

## Done when
In `web/`: `npm run check`, `npx vitest run`, `npm run build` and `npx size-limit` pass. Run Playwright for the settings specs with `PW_CHROMIUM_PATH` pointing at the Chromium under `/opt/pw-browsers` (`ls /opt/pw-browsers`; never run `playwright install`). Also run `npm run e2e:typecheck`. Open the PR titled "X-settings-trim: Settings chunk back under the §10.1 cap". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, head and the byte table. Never merge. Push once per round.
