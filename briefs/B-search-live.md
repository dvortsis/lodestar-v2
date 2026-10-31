# B-search-live: query-scoped live match counts through the percolator
**Model:** Opus: the percolator, the SSE hub, concurrency and content-safety visibility. Runner brief, 2026-10-31. Part 4 of B-search-recovery, following the design in its decisions file. `/security-review` is required: counts can leak hidden content, and registrations are server-side state per session.

## Why
S-search-6's live banner polls `/search` with `limit: 1` every 30 s and compares exact totals. That's wasteful and coarse. #266 recorded a design for an exact "N new matches" feed for the shown query, built on the B9 percolator and the `/events` hub.

## Read first
- **`docs/decisions/2026-10-31-B-search-recovery.md`, "Live counts for a query (part 4)": the design you build.** Deviate only with a reason in your decisions file.
- `docs/decisions/2026-10-31-S-search-6.md` (how the banner works now) and the B9 percolator decisions (`git log --oneline -- crates/lodestar/src/percolator.rs` to find them).
- `docs/architecture.md` §10.4 (Live row), §10.13 (throttle cadence), §20 (visibility).
- `crates/search/src/percolate.rs`, `crates/lodestar/src/percolator.rs`, `crates/api/src/{events.rs,dto/events.rs,search.rs}`, `crates/search/src/recovery.rs` (how `base_query(ast, visibility)` is built).

## Do
1. **Registration:** `POST /search/live {q | ast, hidden, tz}` returns `{id, since}`; `DELETE /search/live/{id}` releases. In memory only; no store rows. The query is stored as `base_query(ast, visibility).query()` with visibility read at registration. A change of "Show adult content" (or any visibility setting) drops every registration.
2. **Matching:** registered queries join the percolator's per-commit run over the new id range, with the same budget and range cap. They add match counts to a per-registration counter, and must not slow saved-search percolation beyond its budget. Measure it.
3. **Delivery:** `event: search_live {id, matched, through}` on `/events`, at most one per subscriber per 5 s, coalescing between.
4. **Bounds:** at most 4 registrations per session (a fifth replaces the oldest), released when the `/events` connection closes or after 10 minutes without one. Auth required. Validate and bound the query size like `/search`.
5. **Visibility (security):** a registered query carries the visibility clauses, so hidden, Adult, quarantined or blocked torrents never count. Test it like #266 did (hidden-only and Adult-only corpora give 0, plus a visibility change dropping registrations).
6. **OpenAPI:** additive, distinct component names; regenerate `openapi.json` and `schema.d.ts`.
7. **UI:** none in this slice. Say in the decisions file what S-search-6's banner should switch to (register on query change, listen for `search_live`, stop polling).
8. **Tests:** register, match on a commit, coalesce under the throttle, the 4-per-session cap, release on disconnect and timeout, delete, and visibility. Condition waits only; no fixed sleeps (use the hub's test clock if there is one).
9. **Decisions file:** `docs/decisions/<date>-B-search-live.md`.

## Own
`crates/search/src/percolate.rs` (only for live entries), `crates/lodestar/src/percolator.rs`, `crates/api/src/{events.rs,dto/events.rs}`, a new `crates/api/src/search_live.rs` (or a submodule of `search.rs`), `web/src/lib/api/openapi.json` and `schema.d.ts` (regenerated only), and your decisions file. No store schema or index format change; if you need either, stop and ask the runner.

## Size
≲ 1,200 changed lines.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the openapi drift check and the search latency (§3) bench pass;
- `npm run check` passes in `web/` (types only);
- `/security-review` is clean.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally. Push once per round of fixes.
