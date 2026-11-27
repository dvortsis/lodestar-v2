# UX-start: the first hour shows progress

**From:** the runner, 2026-11-27T20:57Z. **Model:** Sonnet (Svelte screens per §10; helper). **Branch:** `slice/UX-start`. **PR title:** `UX-start: The first hour shows progress in the pill, Overview and the wizard`.

**Why (owner, 2026-11-15, `docs/owner-decisions.md` §2.77):** "We can't make people wait 6-hours before seeing progress. That's a terrible UX choice." CR-0w (merged) removed the wait. Sampling now starts once the DHT has bootstrapped, after a 10 s start hold, or up to a minute after repeated crashes. D-start (#1148, merged) updated the user guide. The UI still assumes the six-hour wait.

**Read first:**
- `docs/decisions/2026-11-15-owner-no-start-wait.md`, "Proposed slices" item 3 (this slice);
- the UX-start entry in `runner/slices.yaml`;
- `docs/architecture.md` §10 (the UI plan) and §3, "Cold-start reality";
- `docs/ui-glossary.md` (words go through it, and D-start just added safe-restart and restart-record terms);
- `docs/design-tokens.md`;
- `docs/metrics.md` for `/stats` fields (`sampling_blocked`, and the counts you'll show).

**The work**
1. **The status pill** (`web/src/lib/shell/progress.ts`):
   - Drop the six-hour uptime fallback (`STARTUP_SUPPRESSION_S` and the `WINDOW_CLEAR_S` derived from it).
   - "Starting" shows only while bootstrapping, or while `sampling_blocked` says so. After that, show warming up with live counts (found, stored).
   - Read what `sampling_blocked` can be on `main` now, after CR-0w. Handle null, and the start-hold and crash-loop reasons.
   - Keep a fallback for a server that doesn't report `sampling_blocked`, since an older server could be behind a newer UI. Ask the runner if that case can't happen.
   - Update `status-progress.test.ts`.
2. **Overview, first hour** (`web/src/lib/features/insights/overview/**`): instead of empty charts, show the pipeline from `/stats`: routing nodes, samples, metadata fetched, stored. Switch to the normal charts once there's enough data. Use existing tokens and components; `minimalist-visual-style`; no new chart library.
3. **The wizard** (`web/src/lib/features/wizard/**`): replace `wizard_how_cold` in en.json and zh.json; it still mentions the wait. Show live counts on the later steps, since the crawler runs during setup. Fix `WizardPage.test.ts:238` and `:321`, which pin the old text.
4. **Honest expectations:** the index grows over days and weeks. Never promise speed. Use the glossary's words; if you need a new term, add it to `docs/ui-glossary.md`, which is granted for new entries only.
5. **Gates:** in `web/`:
   - `npm run check`, `npx vitest run`, `npm run build` and `npx size-limit` (budget changes only for your own entries in `.size-limit.mjs`, with a reason);
   - `npx playwright test` for any spec you touch, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`;
   - no hex values and no arbitrary values;
   - light and dark, at phone width too.

   Take screenshots of the pill, Overview's first hour and the wizard step for the PR description.

**Owns:** `web/src/lib/shell/progress.ts`, `web/src/lib/shell/status-progress.test.ts`, `web/src/lib/features/insights/overview/**`, `web/src/lib/features/wizard/**`, `web/.size-limit.mjs` (own entries), `docs/ui-glossary.md` (new entries only), `docs/decisions/*-UX-start.md`. Leave `runner/` alone. Ask the runner before touching any other file, including any e2e spec outside these folders.

**Size:** aim for ≲ 800 changed lines.
