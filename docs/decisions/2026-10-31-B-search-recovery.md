# B-search-recovery decisions

Backend for the three deferrals in `2026-10-31-S-search-6.md`: per-clause counts for "No results", an index-state endpoint for "Cold start" and "Index rebuilding" (§10.4 States), and the design of a query-scoped live count, which ships in a follow-up (see the end). Code: `crates/search/src/recovery.rs`, `crates/api/src/search.rs`, `dto/search.rs`.

## Per-clause counts (`recovery` on `/search`)

- **Computed only for an empty first page** (`total` 0, no `offset` or `cursor`) whose query has **two or more top-level clauses**, so a search with results pays nothing and no request flag was added. Removing a query's only clause would browse everything, which isn't a suggestion.
- **A clause is identified by its index** among the AST's top-level clauses (the `of` of a root `and`, else the root alone), the same numbering as `clauses.ts`'s `topLevel` and `rootWithout`. Each entry also carries the reduced AST (`without`) and its canonical text, so the UI never re-parses or rebuilds anything: searching `without` gives exactly the `total` shown (tested).
- **At most 8 clauses** (`MAX_CLAUSES`): structured clauses (the filter strip's chips) are counted first, then free text, so `trackers:>=3` in a long title query still gets its count. `skipped` says how many were left out; `clauses` is returned in query order. The filter strip rarely holds more than 8, and 8 fit in the deadline on the bench corpus (Cost, below).
- **One deadline of 20 ms** (`RECOVERY_BUDGET`) shared by every count of a response, **after** the search: a zero-result search takes what it took before plus at most 20 ms. Each count follows `count.rs`'s rules exactly (exact up to twice the threshold, `total_approx` above it, `≈` per `docs/metrics.md` §1). **A count the deadline cut short anywhere** (estimate, exact pass or the stem fallback's pass) **has no `total`** (absent, never 0 or a rough estimate), and so does every later one. A client that disconnects cancels them like the search; any other failure of the counts drops `recovery` and leaves the (successful) search as it was.
- **Counted on the empty answer's own snapshot** (its lease), at its `now` and time zone, with the same stem fallback a search of the reduced query takes. Counts aren't cached; a cached empty first page recomputes them within the same deadline.
- **Cost:** measured on the search bench corpus (200k torrents, release build, 100 queries per class): each of the 17 classes, with a no-match word appended so it returns nothing, and the recovery pass over its clauses as `/search` runs it (visibility clauses included). The recovery pass's p95 was 0.5–6.0 ms per class (worst: `trackers:>=3` browse, 6.0 ms; word + file clause 4.6 ms), its max 10.1 ms, and no count missed the 20 ms deadline. The zero-result search itself stayed at p95 ≤ 1.1 ms. The measurement patch to `search-bench` wasn't committed (`bench/` isn't this slice's); a gated zero-result class there is a follow-up for its owner.

## Visibility (security)

- **Every count runs `base_query(reduced, visibility)` under the request's own visibility**, so it is exactly what `/search` would return for the reduced query, never more:
  - **Adult while hidden** is a visibility clause ANDed onto every count, not part of the user's query, so removing `type:adult` or `adult:…` can't lift it.
  - **Filter-hidden torrents** (§20.3 `hide`) stay out unless `hidden=true`, or the reduced query itself still asks about `hidden:` (which the full query then did too, so the user already asked for them). Removing a `-hidden:true` clause puts the default `hidden:false` back.
  - **Quarantined, dropped and tombstoned torrents** are never in the index, so no count can include them.
- **Tested:** a query whose only matches are hidden or Adult gets only zero counts for every clause, explicit `type:adult` included; `hidden=true` and "Show adult content" bring them back as `/search` does.
- **Known window, not new:** the per-hit check of the stored Adult class (a correction not yet committed to the index, under 5 s) applies to hits, not counts, as for `/search`'s `total` today. For that window a suggestion can count a torrent the reduced search then leaves out of its hits.
- **Index-state figures are whole-index**, like `/stats`'s `searchable_torrents` and Insights' KPI, which already show them: they say how many torrents the index holds, not which.

## Index state (`GET /search/state`)

- **Lives under `/search`** in `search.rs`, so the router composition (shared) is untouched.
- **Fields the UI binds to** (`SearchIndexState`):
  - `searchable_torrents`: documents in the index (as `/stats`).
  - `cold_start`: fewer than **1,000** (§10.19's full-page `searching_crawling` state; the brief's name).
  - `growing`: fewer than **1,000,000**. **This is what `2026-10-24-S-search-2a.md` (§2.19) called "cold start"**: the "still growing" banner. The UI binds the banner to `growing` and the full-page state to `cold_start`. Both thresholds are the server's now, so `ResultsPanel.svelte`'s `COLD_START_BELOW` can go.
  - `growth_per_hour`: torrents the crawl made searchable, as Insights' Index trend counts them (`insights/stages.rs`): `NEWLY_SEARCHABLE` rows (`docs/metrics.md` §7.7, code 10) with **key 0 only** (imports, key 1, are left out, so a bitmagnet import never shows as growth), in the hours of the last 24 whole UTC hours that the index was measured (`COVERAGE` code 12, index dim 0, > 0 s), divided by that measured time (at most 3,600 s per hour). `growth_hours` is that time in hours, so a half-measured start-up hour counts as half. **Null when nothing was measured**, not 0. No new series.
  - `rebuild`: null, or `{phase, done, total, started_at, from, to, unavailable, error}` from `Serving::progress()`, with the `event: rebuild` phase names.
  - `unavailable`: fields the serving index lacks (`missing_fields()`), whose clauses match nothing until a rebuild.
  - `updated_at`.

## UI follow-up (small)

- **No results:** for each `recovery.clauses[]` with `total > 0`, "Removing `{clause}` gives {total}" (`formatCount`, "≈" when `total_approx`); applying it searches `without`. Entries without `total` show nothing.
- **Cold start / Index rebuilding:** read `/search/state` instead of `/stats` plus the local threshold.

## Live counts for a query (part 4): design, follow-up slice

Not built here, to keep this PR reviewable (the brief's split **b**). The design:

- **Registration:** `POST /search/live {q | ast, hidden, tz}` returns `{id, since}` where `since` is the index watermark at registration. The query is stored in memory only, as `base_query(ast, visibility).query()` with the visibility read at registration; a change of "Show adult content" drops every registration (the client re-registers), so a count never outlives the visibility it was made under.
- **Matching:** the B9 percolator already runs on each NRT commit over the new id range; registered live queries join that run as in-memory entries next to the saved searches (`percolate` on the new range, same budget and range cap), adding their match counts to a per-registration counter. No store rows.
- **Delivery:** an `event: search_live` `{id, matched, through}` on the existing `/events` hub, at most one per subscriber per **5 s** (§10.13's throttle cadence), coalescing counts in between.
- **Bounds:** at most **4 registrations per session** (a fifth replaces the oldest), and each is released when its `/events` connection closes or after 10 minutes without one; `DELETE /search/live/{id}` releases it explicitly.
- **Visibility:** the registered query already carries the visibility clauses, so hidden and Adult torrents can't count; tests mirror part 2's.
