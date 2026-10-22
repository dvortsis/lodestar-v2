# W7e: Chart primitives and the lazy uPlot wrapper
**Model:** Sonnet (helper). **Branch:** `slice/W7e`.

## Own
- `web/src/lib/charts/**` and its tests;
- `web/src/routes/dev/ui/charts/**` (showcase).

## Build (§10.11 data display, §10.12, §10.9.6, §10.18 data tokens)
- Tiny in-house SVG primitives. The target is ≤ 6 KB total gzipped for the SVG set; report the actual size.
  - **KPI:** a value via `format/`, a delta with an icon and text (not colour alone), an optional sparkline, `data-testid`.
  - **CategoryBar:** composition, with categories in the fixed §10.18 order and colours from the data tokens, a text summary and visible dominant-share text, and a shared-scale mode (a `max` prop) for Compare.
  - **HBarList:** a sorted horizontal bar list with labels, values and %. Exact comparison.
  - **Histogram + brush:** explicit bins passed in with edges; zero baseline; empty bins drawn empty; a brush that selects a bin range with mouse and **keyboard** (arrow keys move the edges) and emits the range; labelled edges.
  - **Sparkline:** gaps for null (never zero-filled).
  - **DataTableAlternative:** the accessible table equivalent for each chart (§10.12/§10.13), toggled or visually hidden per the skills.
- **LineChart:** a wrapper around uPlot, **imported lazily** (dynamic `import()`, so it stays out of the first-load chunk; check with size-limit).
  - Theme-aware via tokens (re-reads CSS custom properties when the theme changes).
  - Gaps for missing samples.
  - Absolute dates in tooltips through `format/`.
  - Axis units shown.
  - Resize observer.
  - Reduced motion.
  - Destroys the uPlot instance on unmount.
- **Rules:**
  - every number goes through `src/lib/format` (W8);
  - null → "—", estimates → "≈" (§10.12);
  - no animation that re-animates on update (`motion-ui`);
  - `data-testid` on chart containers and counts.
- No strings of your own beyond props. List any `common_*` keys you need in the PR.

## Common rules (all W7 slices)
- **Read first:**
  - CLAUDE.md "UI rules";
  - `runner/standards.md` (the web checks; §4 budgets);
  - `docs/owner-decisions.md` (design choices);
  - architecture §10.1 (budgets), §10.2 (spacing ramp, type), §10.11 (inventory and rules), §10.12 (data display contracts), §10.13 (accessibility), §10.14 (UX writing), §10.18 (colour roles; never hex);
  - `docs/design-tokens.md`, `docs/ui-glossary.md`;
  - `web/README.md`, `web/src/lib/ui/README.md`, `web/src/lib/format/` (W8; every number goes through it), `web/e2e/README.md` (W6: projects, the OpenAPI mock, axe, keyboard-only helpers, snapshots).
- **ui-design-skills:** `git clone --depth 1 https://github.com/dvortsis/skills /tmp/skills`, then `ui-design-skills/skills/*/SKILL.md`, in its own precedence order. Apply by concept and spirit for Svelte. **The owner's design and §10 come first; the skills only fill gaps.** Relevant: `minimalist-visual-style` (the owner's choice), `application-interface-design`, `motion-ui` (reduced motion), `responsive-layout`, `accessibility` / WCAG 2.2 AA, and `data-visualization-design` plus `analytics-chart-selection` (charts).
- **Stack (already pinned in `web/package.json`; no new dependencies):** Svelte 5 runes, Tailwind v4 + daisyUI 5 via the token mapping (W2), Bits UI 2.20 for overlay and interactive behaviour where it fits, `@tanstack/svelte-virtual` for virtual lists, unplugin-icons (Phosphor), and uPlot (charts only, lazily imported).
- **Tokens only:**
  - no hex, no arbitrary Tailwind values (the lint bans them);
  - spacing from the §10.2 ramp;
  - semantic variant names;
  - geometry and colour come from tokens.
- **Every primitive** gets:
  - vitest + testing-library unit tests for its states and keyboard behaviour;
  - a Playwright showcase page under `web/src/routes/dev/ui/<group>/+page.svelte`, if W6's harness has a dev or showcase route pattern (check `web/e2e/README.md` and the routes); otherwise a vitest-only approach, said in the PR;
  - an axe run on that page in Riverstone light and dark.
  - **Don't commit new snapshot baselines.** Pixel baselines are generated in CI's pinned container; mark new snapshot tests to generate on first run per W6's README, or leave snapshots out and say so.
- **Strings:**
  - Accessible names and visible text come from **props** (e.g. `label`, `closeLabel`) supplied by features.
  - Where a sensible default is needed, use an existing shared `common_*` key from `src/lib/i18n/messages/en.json`. **Only W7ab may add new `common_*` keys.** W7cd and W7e list any they need in the PR and use props meanwhile.
- **Budgets:** primitives are tree-shaken per route. `npx size-limit` must pass, and the first-load budget (≤ 100 KB, JS ≤ 45 KB) can't move because primitives aren't used by any route yet.
- **Don't edit:**
  - anything outside your Own list;
  - `web/package.json` dependencies (scripts only if essential, said in the PR);
  - routes other than your dev showcase pages;
  - other W7 slices' directories (they run at the same time).

## Done when
- In `web/`: `npm run check`, `npx vitest run`, `npm run build` and `npx size-limit` pass, and `npx playwright test` passes for your specs (Chromium at `/opt/pw-browsers`; never run `playwright install`).
- `git fetch origin main` is merged in (no rebase).
- The PR description lists:
  - each primitive with its API (props and events);
  - keyboard behaviour;
  - the states tested;
  - axe results;
  - the bundle size per primitive (from the build);
  - any `common_*` keys or token gaps needed.
