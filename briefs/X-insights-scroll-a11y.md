# X-insights-scroll-a11y (runner brief, 2026-11-13)

The spec is the slices.yaml entry `X-insights-scroll-a11y` (Track U step 4; UI review B3).

**Owns:**
- `web/src/routes/insights/**`;
- `web/src/lib/features/insights/overview/**`;
- `web/e2e/insights*.test.ts`.

If launch shows the scrollable region somewhere else, tell the runner before editing outside these.

**Goal:** /insights passes axe's `scrollable-region-focusable` check at 390, 768, 1366 and 1440 px. Make the region keyboard-reachable (tabindex 0 plus a label, or content that takes focus). Don't just suppress the rule.

**Tests:** an e2e that runs axe on fixture data at those widths. It's a prerequisite for X-ui-guards' fixture-backed axe run.

Starts after X-insights-funnel-fixture merges, because both edit `overview/StageStrip.svelte`. Steward skill; PR title "X-insights-scroll-a11y: …".
