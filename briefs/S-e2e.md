# S-e2e: close the §10.17 Playwright gaps
**Model:** Sonnet (test scaffolding following existing patterns). The runner reviews; an Opus reviewer only if the runner asks. Runner brief, 2026-11-01. Slice entry: `runner/slices.yaml` → `S-e2e`. It's an M5 exit-gate dependency (G-m5).

## Why
`web/e2e/` already has about 80 specs: the smoke matrix, all-themes, reduced-motion, keyboard specs per screen, axe in `a11y.ts`. §10.17 lists what M5 must prove. Nobody has checked the list item by item against what exists. This slice does that audit and fills the gaps.

## Read first
- `docs/architecture.md` §10.17 (the list), §10.19 (brand assets: illustration rules, Save-Data), §10.18 (theme gates, the pre-paint script).
- `web/e2e/README.md`, `web/playwright.config.*`, `web/e2e/smoke.test.ts`, `web/e2e/all-themes.test.ts`, `web/e2e/reduced-motion.test.ts`, `web/e2e/a11y.ts`, `web/e2e/coverage.test.ts`, and `.github/workflows/e2e.yml` (which projects run on PRs and which nightly).
- The runner follow-up on smoke Settings baselines: the smoke mock serves no settings index, so the Settings baselines never render a field (from the #217 review).

## Do
1. **Audit.** Make a table in your decisions file: each §10.17 Playwright and gate item → the spec(s) that cover it, or "gap".
   - Visual snapshots per breakpoint × theme × density.
   - Keyboard-only end-to-end flows: search → open → send; save alert; first-run wizard.
   - The reduced-motion run.
   - axe on key screens.
   - Brand gates that Playwright owns: illustrated states in light and dark at each breakpoint; illustrations hidden when "Show illustrations" is off or `Save-Data` is set.
   - Theme gates: Riverstone light and dark plus a rotating sample of 6 themes per run (all 48 nightly); axe per theme nightly; no flash of the wrong theme from the pre-paint script.
2. **Fill the gaps,** following the existing spec and fixture patterns. Expected candidates:
   - a single keyboard-only **search → open → send** flow, if no spec covers it end to end;
   - the **save alert** keyboard flow;
   - the **wizard** flow, if the wizard specs don't already run keyboard-only end to end;
   - the **Save-Data / illustrations-off** check;
   - a **no-theme-flash** check (the first paint already has the stored theme's colours);
   - the smoke mock's **settings index and one section fixture**, so Settings snapshots render fields.

   That last one changes Settings baselines on purpose: use the marker and check that only Settings images changed.
3. **Don't duplicate:** if a spec already covers an item, cite it and move on. Don't rewrite existing specs except to fix a real gap.
4. **Rules:** condition waits only (≥ 10 s deadlines), no fixed sleeps, and no new flaky selectors (use roles and test ids). Each new spec runs 3× clean locally. Keep CI time in mind: put heavy matrices in the nightly project, not core, and say which project each spec joins.
5. **Decisions file:** `docs/decisions/<date>-S-e2e.md`, with the audit table and what moved to nightly.

## Own
`web/e2e/**` (new specs, fixtures, the mock's settings fixtures), `web/playwright.config.*` (project wiring only), and your decisions file. **Don't edit** app code under `web/src/**`. If a gap needs an app change (a missing test id, say), list it in the PR for a follow-up, don't make it.

## Size
≲ 1,000 changed lines (excluding regenerated PNGs).

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx playwright test` for the new and changed specs (3×) and for core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the audit summary. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
