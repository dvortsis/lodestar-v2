# B-search-recovery decisions

Backend for the three deferrals in `2026-10-31-S-search-6.md`: per-clause counts for "No results", an index-state endpoint for "Cold start" and "Index rebuilding" (§10.4 States), and the design of a query-scoped live count, which ships in a follow-up (see the end). Code: `crates/search/src/recovery.rs`, `crates/api/src/search.rs`, `dto/search.rs`.

## Per-clause counts (`recovery` on `/search`)
- **Computed only for an empty first page** (`total` 0, no `offset` or `cursor`) whose query has **two or more top-level clauses**, so a search with results pays nothing and no request flag was added. Removing a query's only clause would browse everything, which isn't a suggestion.
- **A clause is identified by its index** among the AST's top-level clauses (the `of` of a root `and`, else the root alone), the same numbering as `clauses.ts`'s `topLevel` and `rootWithout`. Each entry also carries the reduced AST (`without`) and its canonical text, so the UI never re-parses or rebuilds anything: searching `without` gives exactly the `total` shown (tested).
- **At most 8 clauses** (`MAX_CLAUSES`), the first 8 in query order; `skipped` says how many more there were. The filter strip rarely holds more, and a narrowed count takes 1–3 ms, so 8 fit in the deadline.
- **One deadline of 20 ms** (`RECOVERY_BUDGET`) shared by every count of a response, inside §3's 30 ms keyword-search target. A clause the deadline reached before any estimate has **no `total`** (absent, never 0), and so does every later one. A clause whose exact pass the deadline cut short keeps the sampled estimate, with `total_approx: true`, as does any count over the sampling threshold (`count.rs`'s rules, `≈` per `docs/metrics.md` §1). A client that disconnects cancels them like the search.
- **Counted on the empty answer's own snapshot** (its lease), at its `now` and time zone, with the same stem fallback a search of the reduced query takes. Counts aren't cached; a cached empty first page recomputes them within the same deadline.
- **Cost:** the zero-result query costs what it did, plus at most 20 ms. On the search bench corpus (200k, one segment), see the PR for the measured p95 of the recovery pass.

## Visibility (security)
- **Every count runs `base_query(reduced, visibility)` under the request's own visibility**, so it is exactly what `/search` would return for the reduced query, never more:
  - **Adult while hidden** is a visibility clause ANDed onto every count, not part of the user's query, so removing `type:adult` or `adult:…` can't lift it.
  - **Filter-hidden torrents** (§20.3 `hide`) stay out unless `hidden=true`, or the reduced query itself still asks about `hidden:` (which the full query then did too, so the user already asked for them). Removing a `-hidden:true` clause puts the default `hidden:false` back.
  - **Quarantined, dropped and tombstoned torrents** are never in the index, so no count can include them.
- **Tested:** a query whose only matches are hidden or Adult gets only zero counts for every clause, explicit `type:adult` included; `hidden=true` and "Show adult content" bring them back as `/search` does.
- **Index-state figures are whole-index**, like `/stats`'s `searchable_torrents` and Insights' KPI, which already show them: they say how many torrents the index holds, not which.

## Index state (`GET /search/state`)
- **Lives under `/search`** in `search.rs`, so the router composition (shared) is untouched.
- **Fields the UI binds to** (`SearchIndexState`):
  - `searchable_torrents`: documents in the index (as `/stats`).
  - `cold_start`: fewer than **1,000** (§10.19's full-page `searching_crawling` state).
  - `growing`: fewer than **1,000,000**, the "still growing" banner's threshold from `2026-10-24-S-search-2a.md` (§2.19). Both thresholds are the server's now, so `ResultsPanel.svelte`'s `COLD_START_BELOW` can go.
  - `growth_per_hour`: the `NEWLY_SEARCHABLE` rollup (`docs/metrics.md` §7.7, code 10) over the last 24 whole UTC hours, averaged over the hours the index was measured in (`COVERAGE` code 12, index dim 0); `growth_hours` is that number of hours. **Null when none was measured**, not 0. No new series.
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
