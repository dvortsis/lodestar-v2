# W7ab: UI primitives (controls, fields, labels, feedback)
**Model:** Sonnet (helper). **Branch:** `slice/W7ab`.

## Own
- `web/src/lib/ui/controls/**`, `web/src/lib/ui/fields/**`, `web/src/lib/ui/labels/**`, `web/src/lib/ui/feedback/**`;
- their tests;
- `web/src/routes/dev/ui/{controls,fields,labels,feedback}/**` (showcase);
- new `common_*` keys in `web/src/lib/i18n/messages/en.json` (and `zh.json` only where the glossary marks them settled).

## Build (§10.11)
- **Controls:**
  - **Button:** primary / secondary / quiet / danger × dense / default, with a loading state that keeps the width and sets `aria-busy`. Disabled vs aria-disabled: choose per the skills and say which.
  - **IconButton:** an accessible name is required.
  - **SegmentedControl:** radio-group semantics, arrow keys.
- **Fields:** Field (label, help text, inline validation message tied with `aria-describedby`, required marker), wrapping Input, Select, Combobox (the ARIA combobox pattern, the base for the query bar later; Bits UI if it fits), Textarea, Checkbox, Radio, Toggle (switch semantics), Slider, and RangeInput with units (two thumbs or two inputs; units shown; values through `format/`).
- **Labels:** Tag/Badge (semantic variants, never colour alone), Chip (removable: the remove button has an accessible name; Backspace/Delete removes when focused), Kbd, Mono (identifier text, Atkinson Hyperlegible Mono), and RelativeTime (relative text from `format/relative`, the absolute time in a tooltip *and* in accessible text, refreshed on a shared ticker, not one timer per instance).
- **Feedback:**
  - **Toast:** a polite live region, a queue, dismissible, honours reduced motion, never steals focus.
  - **Banner:** info / success / warning / error, icon + text, optional action.
  - **Skeleton:** reduced-motion aware.
  - **EmptyState:** a title, text, one next-step action, an optional illustration slot. Don't import brand assets; features pass them in.
- 44 px touch targets at coarse pointer. Focus is always visible.

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
