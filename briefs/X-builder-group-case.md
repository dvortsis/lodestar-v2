# X-builder-group-case (runner brief, 2026-11-21Z)

Kind: core, model opus. Workstream ws-web. Deps: X-facet-group-case (#1033, merged d6c83c56).

## Why
#1033 gave facet values a server display form (FacetValue.label) and the facet rail shows it via
`valueLabel(name, v, locale, label)`. The Search Builder's value options
(web/src/lib/features/search/assist/BuilderRow.svelte:54) call `valueLabel(field, v, locale)` with
no label, so a release-group value there would read lower-case ("rarbg") where the rail reads "RARBG".

## Step 0 (send to the runner before building)
1. Find where the Builder's value lists come from (`spec.values`, /search/fields) and whether a
   release-group value can reach `valueOpt` at all (a fixed list, a suggestion, or a value typed in).
2. If no group value can reach it, say so with the evidence; the slice closes as a record-only PR
   (decisions file plus its slices.yaml entry), no code change.
3. Otherwise propose the smallest change that shows the same label the rail shows, with no new
   request if the data is already on the client, and name the files.

## Rules
- Owns (refine at step 0): web/src/lib/features/search/assist/**, web/.size-limit.mjs (the Builder
  entry only), docs/decisions/*-X-builder-group-case.md, its own slices.yaml entry and ws-web placement.
- The query text keeps the lower-cased value; only the shown label changes. Same label rule as #1033
  (ASCII re-casing only); never re-derive it on the client from a name.
- Builder (on demand) is 8.45 kB, measured 8.43 kB. A raise only as the measured size rounded up to
  0.01 kB, with numbers sent first.
- Unit tests for the label path; no fixed sleeps; waits ≥10 s.
- PR title "X-builder-group-case: <title>", branch slice/X-builder-group-case. Push once per round;
  send "ready <pr> <sha>" when green. Follow .claude/skills/steward/SKILL.md; never merge.
