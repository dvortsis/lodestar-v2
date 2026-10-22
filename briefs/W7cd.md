# W7cd: UI primitives (overlays, lists, tables, tree)
**Model:** Sonnet (helper). **Branch:** `slice/W7cd`.

## Own
- `web/src/lib/ui/overlays/**`, `web/src/lib/ui/lists/**`, and their tests;
- `web/src/routes/dev/ui/{overlays,lists}/**` (showcase).

## Build (§10.11, §10.13, §10.5)
- **Overlays (on Bits UI where it fits):**
  - **Tooltip:** hover and focus, Esc closes, no essential content only in tooltips.
  - **Popover.**
  - **Menu:** the menu-button pattern, typeahead, arrow keys.
  - **Dialog:** a modal focus trap that restores focus on close, Esc closes, a labelled title.
  - **Drawer/Sheet:** non-modal Drawer for the detail view (§10.5: keeps the list visible, focus moves to its title, Esc closes and returns focus to the invoking row, resizable with a persisted width via a prop callback, not storage inside the primitive); modal Sheet for narrow screens.
  - Only Dialog and the modal Sheet trap focus (§10.13: nothing else traps focus).
  - Reduced motion is honoured.
- **Lists and tables:**
  - **VirtualList** (`@tanstack/svelte-virtual`): listbox/grid semantics with a roving tabindex, `aria-setsize`/`aria-posinset`, and keyboard j/k-ready (emit events; the keyboard registry belongs to W9). It keeps focus when items are prepended (live inserts) and supports a "load more" sentinel for cursor paging.
  - **Table:** sortable headers (`aria-sort`), a sticky header, a column chooser (a popover with checkboxes, as a controlled prop), selection (checkbox column with shift-range, as a controlled prop), and virtualised rows for large sets.
  - **Tree:** virtual, the WAI-ARIA tree pattern (arrows, Home/End, type-ahead, expand/collapse with * for siblings), checkboxes with tri-state parents for "Send selected files", and folder sizes passed in, already formatted. It must handle 100k nodes smoothly (flattened visible-row model). The Web Worker for files above 2k is a feature concern; keep the tree's data model worker-friendly (plain arrays) and say so.
- No strings of your own beyond props. Use existing `common_*` keys only; list needed ones in the PR.

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
