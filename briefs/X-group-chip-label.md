# X-group-chip-label (runner brief, 2026-11-22Z)

Kind: core, model opus. Dep X-builder-group-case is merged (#1037). Low priority, UX polish.

## What
Release-group values are lower-cased in three places, while the facet rail shows the display form (`FacetValue.label`, from #1033):
- the query chips (`query/clauses.ts`, `tagValue`);
- the Builder rows (`builder.ts`, `rowOf`);
- after a facet click (`TermGroup.svelte`).

When the client already holds a label for that value, show it, from the facet rail's data or the label passed with the click.

## Rules
- **Never re-derive a label from a name or invent casing.** No label means today's lower-case text.
- **The query text stays lower-cased:** the URL, the copy-query action, saved searches and the submitted query. Only the displayed chip or row text changes.
- **The S1-20 rule applies to any label shown:** ASCII re-casing of the value only. If a label fails that check, drop it and show the value.
- Labels are text, never HTML. Watch the copy-query, share and history paths: none of them may pick up the label.
- **Size:** keep route JS within the limits in `web/.size-limit.mjs`. Measure with `npm run size:ci` (#1041 has merged). A raise is allowed only as the measured size rounded up to 0.01 kB, with the numbers sent to the runner first.
- **Tests:**
  - unit tests for each of the three places (label known, label unknown, label failing the check);
  - one that the query string is unchanged;
  - an e2e assertion if an existing spec covers chips.
- **Owns:** `web/src/lib/features/search/query/**`, `web/src/lib/features/search/assist/**`, `web/src/lib/features/search/facets/**`, `web/.size-limit.mjs`, `docs/decisions/*-X-group-chip-label.md` and this slice's own `slices.yaml` entry. Never beside X-zero-rescue.

## Step 0 (send to the runner before building)
1. Where each of the three places gets its text, and where the label would come from. Which state holds the rail's labels, and does it outlive a facet refresh?
2. Behaviour on a fresh page load from a URL, when the rail hasn't loaded yet.
3. Changed files, the estimated lines, and the size effect.

**Branch and PR:** branch `slice/X-group-chip-label`, PR title "X-group-chip-label: <title>". Push once per round, then send "ready <pr> <sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
