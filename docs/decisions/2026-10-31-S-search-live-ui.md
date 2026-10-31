# S-search-live-ui decisions: the live banner listens instead of polling

Runner brief, 2026-10-31. Builds on `2026-10-31-B-search-live.md` (the API) and `2026-10-31-S-search-6.md` (the banner). Code: `web/src/lib/features/search/live/`.

- **Registered, not polled.** When the rows shown change, `LiveHost` calls `live.follow(query)`: `POST /search/live {ast, q: null, hidden, tz: null}` (the search's AST and `hidden`), then `DELETE` for the previous id. `live.start()` listens for `search_live` on the shared `/events` store (`events.on`), so no second stream. "N new results" is `matched` for the current id only; other ids are ignored.
- **Same query, no churn.** `follow` skips a query it already holds (key = AST + `hidden`; sort and `exact` don't change what is counted). Show passes `force`: it reloads, releases the id and registers again, so the count starts at 0.
- **Re-register** on any `resync` and on a `settings` event for `content` (the server drops every registration for the latter, and a restart for the former). The store tells a handler nothing about the resync reason, so a `lagged` resync re-registers too and loses the count so far; that only under-counts a banner that Show would refresh anyway. Not covered: a restart on a stream that never received an `id:` sends no `resync` and the store is read-only here, so that banner stays quiet until the query changes.
- **Pause.** Scrolled deep, or a selection held: the count is kept but not shown; the existing 30 s tick shows it once unpaused. One throttled polite announcement per interval, as before.
- **Release** when the query changes, when the banner unmounts (`DELETE`), and on `pagehide` with a keepalive `DELETE` (the API client has no keepalive option, `lib/api` is not ours, so it is one raw `fetch`). The server's 30 s grace and 60 s idle cover failures.
- **Fallback, a deliberate difference from `B-search-live`'s item 5.** If registration fails (the 429 cap, offline, search disabled), that query falls back to S-search-6's probe (`limit: 1`, exact totals only, every 30 s, visible tab). The runner's brief asks for this; the B-search-live note says to show no banner. A 404 on `DELETE` is ignored.
- **Size.** First load of `/search` is unchanged; the live chunk (still on demand) grew from 2.21 KB to 2.74 KB gzipped, and its limit moves from 2.42 KB to 3.02 KB (measured + 10%). No other limit changed.
- **Mock.** `e2e/mock/openapi-mock.ts` answers `releaseSearchLive` with 204 by default, as it does for `listSavedSearches`; `DELETE /search/live/{id}` has no example, and every search spec now releases on exit.

## Carried from #271
- Zero-result lead "Remove a filter to widen the search:" shows only with filter clauses. A words-only query shows `search_results_none_words`, with the recovery counts if the server sent any. Tested.
- `IndexState.test.ts` restores the "no `/h`" test for `growth_per_hour` null, 0 and 0.5.
- The search vitest suite ran 10× clean (595 tests each). The one failure seen earlier in the session was `QueryBar.test.ts`'s Syntax dialog, in a run that shared the machine with another job; it did not recur in 10 solo runs plus the full suite, so no `ResultsPanel` flake was reproduced.
