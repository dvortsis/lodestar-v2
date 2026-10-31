# B-search-live decisions: query-scoped live match counts

Part 4 of B-search-recovery, built to the design in `2026-10-31-B-search-recovery.md` ("Live counts for a query"). Code: `crates/api/src/search_live.rs` (registry, matching entry point, `POST /search/live`, `DELETE /search/live/{id}`), `crates/api/src/events.rs` (per-session delivery), `crates/search/src/percolate.rs` (count-only entries), `crates/lodestar/src/percolator.rs` (the run). Spec: §8 (percolation), §9 (`/events`), §10.4 (Live row), §10.13 (throttle), §20 (visibility).

## As designed
- **Registration** `POST /search/live {q | ast, hidden, tz}` → `{id, since}`; `DELETE /search/live/{id}` releases it. In memory only, no store rows, no index change. The stored query is `base_query(ast, visibility).query()` with visibility read at registration; `q`/`ast` and `tz` go through `/search`'s own `request_ast` and `tz_param`, so the same parser bounds (2,048 bytes, 64 clauses, depth 16) and errors apply. Auth required (`auth::protected`), and `same_origin_json` on both, as every state-changing route.
- **Matching:** the percolator runs live queries as count-only `Due` entries of `percolate` over the new id range, with the run's budget and `max_range`, after its saved searches (see below).
- **Delivery:** `event: search_live {id, matched, through}` on `/events`, at most once per 5 s per connection per registration, counts in between combined.
- **Bounds:** 4 per session (a fifth replaces the oldest); released when the session's last `/events` connection closes, after 10 minutes without one, or by `DELETE`.

## Decided here
- **Live queries run after the saved searches, on what's left of the same budget.** A run percolates the saved searches, records and announces their alerts, then runs the live queries with the same `CancelToken`. So an alert never waits on a live query, and a run never goes past its budget for them: once it's spent they're `more`, and the next run (straight away, now with no saved searches due) takes them. **Measured** (`percolator::tests::live_queries_follow_the_saved_searches`, debug build, 2,000 new torrents): 200 saved searches took 4.2 s (mostly recording 200 × 1,000 alerts), and 256 live queries (the most a hub holds) then took 12 ms.
- **`matched` is cumulative since registration**, not a delta: a missed or combined event loses nothing, and every tab of a session can show the same number. Throttled events only go out when `matched` changed.
- **Per session, never broadcast.** `search_live` events skip the ring and the broadcast (`publish` refuses them) and carry no `id:`, so they're never replayed: each client's own task reads its session's counts from the registry. A new topic, `search_live` (the eighth, so the `u8` topic set is now full), lets a client opt in or out with `?topics=`; with no `?topics=` it gets them like every topic.
- **Ids are random** (16 hex digits), not sequential, so they say nothing about other sessions; another session's id is a 404, the same as none.
- **No ids while Adult is hidden:** `since` and `through` are null for a registration made while "Show adult content" is off, as `torrents`' `max_id` is (§20.9): ids are sequential, so the gap between two would tell how many hidden torrents arrived.
- **A visibility change drops registrations lazily but before any count leaves.** Each registration remembers the Adult setting it was made under; every read of the registry (register, release, the percolator's due list and its record, each delivery) first drops those made under another setting. Filter-hidden torrents follow the stored `-hidden:yes` clause; quarantined, dropped and hash-listed torrents are never indexed (§20), so they can't count.
- **A total cap of 256** (4 for each of the hub's 64 streams), past which a new session's registration is a 429 `too_many_live_queries`. A session already at 4 still replaces its oldest.
- **`since` is the searcher's snapshot**, the highest id search can see now, not the committed watermark, so what's counted is what the page couldn't have shown yet.
- **Live queries honour `tz`:** `percolate::Due` gained `utc_offset` (saved searches still run at 0) and `count_only`, and `Matched` gained `count` (every match, past `keep`).
- **"Closes" means the session's last connection.** A registration isn't tied to one `/events` connection (the POST can't name one), so a session with two tabs keeps both tabs' registrations until its last connection closes; the 4-per-session cap and `DELETE` bound the rest.

## What S-search-6's banner should switch to (no UI in this slice)
1. On each query change (and when a page with a query opens), `DELETE` the previous registration if there is one, then `POST /search/live` with the query shown (`ast`, `hidden`, `tz`) and keep its `id`.
2. **Re-register on every `/events` `open`** (a reconnect released it) and after a `settings` event for the content section (a visibility change released it). A 404 on `DELETE` is fine to ignore.
3. Listen for `search_live`; when `id` is yours, show "N new results" with N = `matched`. It's exact, so drop the `total_approx` checks.
4. Stop the 30 s `limit: 1` polling. Keep the rest of the banner (pause while scrolled, Show reloads and clears, throttled announcement). Show re-registers, so the count starts again from 0.
5. On a 503 (`events_unavailable` or `search_disabled`) or a 429, show no banner rather than fall back to polling.
