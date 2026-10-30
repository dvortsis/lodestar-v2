# S-settings-reconcile: one Settings implementation, within budget
**Model:** Opus. It's the shared Settings engine every later Settings screen builds on: S-settings-2, S-logs, S-about, S-updates and M-security-settings. Lane S of the master plan (`docs/decisions/2026-10-30-master-plan.md`); runner/slices.yaml `S-settings-reconcile`.

## Base
- Branch `slice/S-settings-reconcile` from `origin/main`. Open a PR to `main` and follow `.claude/skills/steward/SKILL.md`. The runner merges.
- Not for v0.5.1: `release/v0.5.1` is already cut.

## The situation
There are two parallel Settings implementations:
1. **The owner's `848e10a0`** (`origin/work/soak-settings`, on the new lineage, merge-base `5e1a53c0`): "preserve prepared screens and validation status". It's about 77 files and +2.4k lines against main, and it **failed 5 size budgets**. Its prerequisite `9c065fcb` is already on main (L6).
2. **The paused runner slices S-settings-1 and M-settings-form-3**, from the old history: the Settings route and registry, core screens, the form engine, and the save pipeline as one on-demand edit chunk. Each is saved as a patch: `git show origin/runner-state:ports/S-settings-1.patch` and `ports/M-settings-form-3.patch`, with `ports/README.md`.

## Do
1. **Base on `848e10a0`.** Merge `origin/work/soak-settings` into your branch, resolving against main. Then take from the two patches whatever 848e10a0 lacks and is still right: the route measurements and on-demand gating from S-settings-1, and the save pipeline as an on-demand edit chunk from M-settings-form-3. **One implementation, no duplicates.**
2. **Budgets: fix them by splitting code, never by raising limits.**
   - All of `npx size-limit` must pass with `web/.size-limit.mjs` limits unchanged. A new entry for a new chunk is fine if its limit follows the "measured + 10%" rule and the decisions file says why.
   - Use lazy imports and `import.meta.glob` screen registries, and keep the save pipeline off the initial route.
3. **Owner check:** if loading the save pipeline on demand conflicts with an owner decision (owner-decisions, §10.10, the design tokens), **stop and message the runner.** That decision goes to the owner.
4. Keep the screen registry contract the later slices rely on: `routes/settings/[section]`, with screens found through `import.meta.glob` over `web/src/lib/features/settings/*/screen.ts`. Messages go only in per-feature `messages/{en,zh}.json`.
5. Tests:
   - vitest for the form engine, validation status and preserved screens;
   - Playwright Settings specs at 390 and 1366 px, with no `waitForTimeout`;
   - all-themes if any visual changes.

## Own
`web/src/lib/features/settings/**`, `web/src/routes/settings/**`, `web/.size-limit.mjs` (entries only, per item 2), Settings e2e specs under `web/e2e/**`, and `docs/decisions/<date>-S-settings-reconcile.md`.

## Done when
In web/: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` and the Settings Playwright specs pass. Chromium is at /opt/pw-browsers; never run playwright install.
- The steward self-review is done.
- If the PR is over ~1,500 lines, its description says why; split it if it's over ~3,000.
- Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg). No model identifiers in commits, PRs or code. Never merge.
