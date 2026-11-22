# X-openapi-400s-tidy decisions

Runner brief, 2026-11-22Z. This takes the nits from #1053's review (recorded in `docs/decisions/2026-11-22-runner-records-r11.md`). Tests and docs only: no change to `crates/api/src/**`, and the OpenAPI document isn't regenerated.

No new error code, dependency or `unsafe`.

## A stale exception now fails the test

`every_query_route_declares_a_400` (`crates/api/tests/openapi.rs`) excuses the routes in `QUERY_WITHOUT_400`. Before, an entry that matched no route (after a rename or a removal) was ignored.

- Each entry must now match one operation in the generated document, by method and path, and that operation must still take at least one `in: query` parameter. A route that loses its query makes its entry fail.
- Each entry must be listed once; a copy-pasted duplicate fails.
- Every stale entry is reported by method and path, in one failure.
- **Checked by hand, not committed:** with `("GET", "/nope", "x")` added, the test fails and names `GET /nope`. With the real entry renamed to `/feeds/{idx}`, it fails and names `GET /feeds/{idx}`. With the real entry copied, it fails with "listed more than once". With the entry pointed at `/openapi.json`, which takes no query, it fails with "no operation that takes a query".

## The `/feeds/{id}` reason

The reason said "a bad id a 404". The route reads its id as a `String` through `crate::extract::Path`, so a path segment that isn't valid UTF-8 is still `400 bad_path`, answered before the handler runs. Any readable id that isn't a saved search's is the one `404 feed_not_found`.

- The reason now says the route declares no query 400, apart from `bad_path`.
- `bad_path` stays undocumented here for the reason #1053 gave: it waits for a slice that covers every path route.

## Correction to `2026-11-22-X-api-openapi-400s.md`

Its "Not in this PR" section says `bad_path` would be listed "on only these three" without naming the third. Read against that file's table, the three are `GET /collections/{id}` and `GET /saved-searches/{id}/alerts`, both named there, and `GET /torrents/{hash}/files`, whose `{hash}` segment is a path parameter too (it answers `bad_hash` for a malformed hash, and `bad_path` only when the segment can't be read at all). `GET /feeds/{id}` is a fourth query route with a path parameter, and every path operation can answer it. The point stands: no path route documents `bad_path` yet. That file isn't edited; this note is the correction.
