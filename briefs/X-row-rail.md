# X-row-rail (runner brief, 2026-11-28)

**Slice:** X-row-rail. Sonnet, UI.
**Branch:** `slice/X-row-rail` from current `main`.
**PR title (exact):** `X-row-rail: Facet order, a two-tier result row, presets, typed dates and badge filters`
**Owns:** `web/src/lib/features/search/facets/**`, `web/src/lib/features/search/results/Row*`, this
slice's own e2e specs, its en and zh message keys, `docs/decisions/*-X-row-rail.md`, and the
`.size-limit.mjs` lines of the chunks it grows (with the runner's grant). Not
`results/RowActions.svelte`'s keyboard behaviour (that's X-rowactions-arrowdown, which never runs
beside this slice), and not `search/toolbar/**` (#1183).

## Source
`runner/slices.yaml` X-row-rail, from the UI review in `docs/review/2026-11-13-ui-ux/` (U10, U11,
I6, U15, 02 §4c), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`.
- **Badges:** with five on a row, safety and verification never give way and the rest collapse
  into a +N chip (the runner's call, owner item 28; record it in the decisions file).
- **Within §2.69 item 17.2:** no performer facet (§2.37 Q4), versions grouped, no third-party
  images (§2.66).

## Do
1. **Step 0.**
   - Read the review items and the §10.4 plan.
   - List what you'll change for each item, and the files.
   - Measure the current sizes of the chunks you'll touch, and estimate the growth of each.
   - Send it to the runner, with the budget grants you need, and wait for the reply.
2. **Build it.**
   - Follow the UI rules in CLAUDE.md: tokens only, no hex, no arbitrary values, and spacing on
     the §10.2 ramp.
   - Keyboard and screen-reader behaviour per §10.13. A badge you can click to filter is a real
     button with a label.
   - Nothing moves when async data arrives.
3. **Tests.**
   - Unit tests for the facet order, the presets, typed dates and +N collapse, with safety and
     verification always kept.
   - An e2e for the two-tier row at 390, 1,024, 1,440 and 2,560 px, and for clicking a badge to
     filter.
   - Condition waits only, with deadlines of at least 10 s.
4. A decisions file, the CLAUDE.md `web/` checks, `.claude/skills/steward/SKILL.md`, and report
   `ready <PR> <sha>`.
