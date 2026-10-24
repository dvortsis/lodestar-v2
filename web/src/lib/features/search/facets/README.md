# features/search/facets

The search page's facet rail (S-search-2b; architecture §10.4, §10.15). Decisions: `docs/decisions/2026-10-24-S-search-2b.md`.

## Files

- `toggle.ts`: facet clicks as AST edits, pure and exhaustively tested.
  - A facet's own clauses are the server's (`crates/search/src/facets.rs`, `Facet::owns`): the top-level conjuncts built only from its field.
  - A toggle rewrites them into at most two clauses, in place of the first: the selected values (`res:1080p,2160p`) and the excluded ones (`-res:480p`).
  - Ranges and dates replace the field's top-level range clause.
- `bins.ts`: histogram bins (`[lo, hi)`) to and from the query's inclusive ranges, for the brush.
- `facets.svelte.ts`: the counts store (`POST /facets`).
  - The latest request wins, the same request is never sent twice, and the last counts stay while a new request is out.
  - `lease` is the results' first `next_cursor` (`results.lease`), so the counts match the rows.
- `FacetsHost.svelte`: loads with the results (`routes/search/+page.svelte`, in `#search-rail`) and places the rail by breakpoint:
  - **x-large and up:** the rail in flow;
  - **large:** "Filters (N)" opens a left sheet, and a click applies at once;
  - **medium and small:** a bottom sheet that stages its clicks, with "Show N results" from `/facets`' `total` for the staged query. Nothing is written until Show.
  - The rail's chunk, its counts and the sheet load only where the rail shows: below x-large, on the first open.
- `FacetRail.svelte` and `facets.css`: the groups in §10.4's order (Type, Discovered, Size, Extension, Resolution, Seeders, Trackers reporting), then "More filters".
  - Every count has a `data-testid` (`facet-<name>-<value>-count`).
  - "≈" appears only on `approx`, with one sampled note per panel.
- `TermGroup.svelte`: a term facet's values.
  - Checkbox to select, minus to exclude, Clear.
  - "Show more" (up to 100 values) with a search box over them.
- `RangeGroup.svelte`: a histogram facet on `charts/Histogram.svelte`.
  - The brush works by pointer and keys.
  - Discovered adds presets above it and a backfill note under it, and snaps a brush to whole days.
  - Release year sits under More filters.
- `SwarmGroup.svelte`: Seeders and Trackers reporting.
  - Until swarm health is in the index (M4) the buckets are null: "—", "Not available yet" and why, never zeros.
  - Once they're sent: presets ≥ 1 / ≥ 5 / ≥ 50.

## Not yet (follow-ups)

- "Include never-scraped": the query language has no clause for it yet.
- Junk ("N hidden · Show"): the server doesn't serve it.
- Category (`cat:` shares): a share range, not a value toggle.
- The other "More filters" groups the server doesn't count yet (Season/Episode, File count, Largest file, Composition thresholds, In library, Network, Source, Verified, Official, Hidden by filters).
