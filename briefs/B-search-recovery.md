# B-search-recovery: per-clause counts, index state, query-scoped live counts
**Model:** Opus: search (query AST, counts, the percolator) and content-safety visibility. Runner brief, 2026-10-31. Backend for the three deferrals S-search-6 (#256) named in `docs/decisions/2026-10-31-S-search-6.md`. `/security-review` is required, because counts can leak the existence of hidden content.

## Why
§10.4's States table asks for three things the API can't give yet:
- **No results:** "Removing `trackers:>=3` gives 1,240". The UI needs, for an empty result, the total each query would have with one clause removed.
- **Cold start / Index rebuilding:** banners need the index size, its growth rate, and the rebuild's progress (`RebuildProgress`, `Serving::progress()`, `missing_fields()`; architecture around line 2640).
- **Live:** S-search-6's banner polls `/search` with `limit: 1` and compares exact totals. An exact "N new" for the shown query is better served by the B9 percolator.

## Read first
- `docs/decisions/2026-10-31-S-search-6.md` (the deferrals and what the UI will bind to) and `web/src/lib/features/search/live/**`, `results/**` (how the banner and the empty state work today).
- `docs/architecture.md`:
  - §10.4, the States table (around line 455) and the cold-start row (line ~465);
  - the search counts and `≈` rules (§3 search latency budget, §10.4 counts, sampling);
  - the rebuild section (`RebuildProgress`, around lines 2640–2700);
  - §10.13 (announcement throttling, for the live feed's cadence);
  - §20 visibility: adult, quarantined, blocked and filtered content.
- `crates/api/src/search.rs` (`/parse`, `/search`), `crates/search/src/{handle.rs,rebuild.rs,percolate.rs}`, `crates/lodestar/src/percolator.rs`, `crates/api/src/events.rs` and `dto/events.rs`.

## Do
1. **Per-clause counts.** For a result whose total is 0 (or a request flag asks for them), return, for each top-level removable clause, the total with that clause removed.
   - Identify clauses through the AST, with a stable clause id or span the UI can use to remove the same clause. Don't make the UI re-parse to find it.
   - Bound the cost: at most N clauses (pick N, say 8, and say why), a shared deadline inside the §3 search budget, and counts marked `≈` when sampled or cut short. A count that missed the deadline is absent, never 0.
   - Prefer computing them on `/search` only when the total is 0, so normal searches pay nothing. Measure the p95 cost on the bench corpus.
2. **Visibility (security-relevant).** Every count runs under exactly the same visibility as the user's search: filters, the adult setting, quarantine, blocklists and tombstones. A removed clause never widens visibility: removing `adult:true` or a filter-bypassing clause can't reveal hidden counts. Test it: a corpus where the only matches are hidden gives no suggestion and no non-zero count.
3. **Index state.** An endpoint (or an additive field on an existing status endpoint, if one fits) with:
   - the searchable torrent count and the growth per hour (from existing rollups, no new series invented; cite `docs/metrics.md`);
   - the rebuild state from `Serving::progress()`: phase, done/total, started_at, and `unavailable` fields;
   - a `cold_start` flag per the §10.4 / line 1311 definition (< 1k torrents), so the UI doesn't pick the threshold.
4. **Live counts for a query.** Through the B9 percolator and the SSE hub: the UI registers its current query and receives "n new matches since" events, throttled (at most one event per few seconds per subscriber) and bounded (a cap on registered queries per client, released on disconnect).
   - Reuse the existing percolator and event plumbing. If that's too large for this slice, ship parts 1–3 and write part 4 up in the decisions file with the design; say so in the PR.
   - Same visibility rule as part 2.
5. **OpenAPI:** additive fields and new types with distinct names (the #255 collision class). Regenerate `openapi.json` and `schema.d.ts`.
6. **Tests:** clause ids round-trip to the same removal; counts for 0-result queries; the deadline gives absent, not 0; `≈` when sampled; the hidden-content cases from part 2; index state while rebuilding and at cold start; the live feed's throttle, cap and disconnect release. Condition waits only, never fixed sleeps.
7. **Decisions file:** `docs/decisions/<date>-B-search-recovery.md`, including the exact fields a small UI follow-up should bind to.

## Own
`crates/api/src/search.rs` and any new api module for index state, `crates/search/src/**` only where counts or the percolator need it, `crates/lodestar/src/percolator.rs`, `crates/api/src/{events.rs,dto/events.rs}` for part 4, `web/src/lib/api/openapi.json` and `schema.d.ts` (regenerated only), and your decisions file. No store schema change and no on-disk index format change; if you need either, stop and ask the runner.

## Size
≲ 1,500 changed lines. If it grows, split: **a** = parts 1–3, **b** = part 4.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the openapi drift check and the search latency (§3) bench pass;
- `npm run check` passes in `web/` (types only);
- `/security-review` is clean on the diff.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally. Push once per round of fixes.
