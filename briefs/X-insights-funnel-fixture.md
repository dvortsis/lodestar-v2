# X-insights-funnel-fixture (runner brief, 2026-11-13)

The spec is the slices.yaml entry `X-insights-funnel-fixture` (Track U, from the UI/UX review of 2026-11-13: B4, re-ranked Important). Read its notes in full.

**Owns:** exactly the slices.yaml `owns` list:
- `bench/src/fixture.rs`;
- `bench/tests/fixture.rs`;
- `web/src/lib/features/insights/overview/StageStrip.svelte`;
- `web/src/lib/features/insights/stages/**`;
- `web/src/lib/features/insights/messages/**`.

**Do:**
1. **Seed the fixture.** Add FUNNEL_EVENTS rollups to `bench/src/fixture.rs`, so the overview's Fetch stage and the funnel agree: fetched, plus each loss reason, consistent with the fixture's `cohort_hour` rows. Add a test in `bench/tests/fixture.rs` that the two agree.
2. **Label the time base.** StageStrip states the time base it uses.
3. **Empty state.** The Fetch stage page (`stages/StagePage.svelte`) shows an empty state when there are no fetches, not ten rows of 0.

**Rules:**
- Tokens only; spacing on the §10.2 ramp; en and zh strings.
- Tests wait on conditions with deadlines ≥ 10 s; no fixed sleeps.
- `npm run check`, vitest, build and size-limit; the fixture e2e if it's touched.
- The live-crawl check of the counter stays open; note it in the decisions file.

Next in the same session: X-insights-scroll-a11y. It overlaps `overview/StageStrip.svelte`, so it starts after this PR merges.

Steward skill; PR title "X-insights-funnel-fixture: …".
