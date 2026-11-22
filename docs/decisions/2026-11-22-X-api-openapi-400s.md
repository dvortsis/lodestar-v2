# X-api-openapi-400s decisions

Runner brief, 2026-11-22Z. This follows up "Not in this PR" in `docs/decisions/2026-11-21-X-api-openapi-repeated.md` (#1021): the `Query<T>` routes that declared no 400, or only part of one.

No new error code, dependency or `unsafe`. Only `responses(...)` attribute lines change; no handler does anything different.

- **Who decided:** choices marked "runner" are the runner's calls. The rest are the slice session's, for the runner to confirm.

## What every `Query<T>` route can answer

`crate::extract::Query<T>` reads the query in two steps, before the handler runs:

- **A known parameter sent more than once** is `400 repeated_parameter`, naming it in `field`. "Known" means a field of `T`, so every route below can answer it, whatever its values.
- **A value of the wrong type, or a required field that's missing,** is `400 bad_parameter`, naming the parameter in `field` where serde gives a path.
  - A struct whose fields are all strings can't fail this step. On `/digest/preview` the only `bad_parameter` is the handler's own check on `period`.

So no `Query<T>` route is exempt, and none is listed as one.

## The 400 descriptions

Each list holds the codes that the route's own code paths can return as a 400, read from those paths (runner). Each closes as #1021's do: the parameter at fault is in `field` where there is one, and `/parse`'s `query_syntax` also gives `start` and `end`.

| Route                                                                                     | Before        | Now                                                                                                                                |
| ----------------------------------------------------------------------------------------- | ------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `GET /parse`                                                                              | no 400 listed | `repeated_parameter`, `bad_parameter` (`natural`), `query_syntax`, `natural_too_long`                                              |
| `GET /digest/preview`                                                                     | no 400 listed | `repeated_parameter`, `bad_parameter` (`period` isn't daily or weekly)                                                             |
| `GET /torrents/{hash}/files`                                                              | no 400 listed | `repeated_parameter`, `bad_parameter` (`offset`, `limit`), `bad_hash`                                                              |
| `GET /collections`                                                                        | no 400 listed | `repeated_parameter`, `bad_parameter` (`hidden`)                                                                                   |
| `GET /collections/{id}`                                                                   | no 400 listed | `repeated_parameter`, `bad_parameter` (`after`, `hidden`, `limit`)                                                                 |
| `GET /saved-searches/{id}/alerts`                                                         | `bad_cursor`  | `repeated_parameter`, `bad_parameter` (`limit`), `bad_cursor`                                                                      |
| `GET /user-items`                                                                         | `bad_cursor`  | `repeated_parameter`, `bad_parameter` (a wrong type, or a `label` the store refuses), `bad_cursor`                                 |
| `GET /user-items/labels`                                                                  | no 400 listed | `repeated_parameter`, `bad_parameter` (`hidden`)                                                                                   |
| `GET /filters/queue`                                                                      | no 400 listed | `repeated_parameter`, `bad_parameter` (`limit`), `bad_limit` (outside 1–1000), `bad_cursor`                                        |
| `GET /filters/tombstones`                                                                 | no 400 listed | as the queue's, with `adult` among the `bad_parameter`s                                                                            |
| `GET /discover`                                                                           | no 400 listed | `repeated_parameter`, `bad_parameter` (`limit`), `unknown_type`, `bad_cursor`                                                      |
| `GET /settings/export`                                                                    | no 400 listed | `repeated_parameter`, `bad_parameter` (`preferences`)                                                                              |
| `POST /settings/import`                                                                   | no 400 listed | `repeated_parameter`, `bad_parameter` (`dry_run`), `bad_json`, `bad_body`                                                          |
| `GET /suggest`                                                                            | no 400 listed | `repeated_parameter`, `bad_parameter` (`pos`, `limit`, `hidden`), `query_too_long`, `bad_pos`                                      |
| `GET /insights/overview`                                                                  | no 400 listed | `repeated_parameter`, `bad_parameter` (a wrong type, or `compare` not 0 or 1), `bad_range`                                         |
| `GET /insights/events`                                                                    | no 400 listed | `repeated_parameter`, `bad_parameter` (`from` or `to` missing or not an integer), `bad_range`                                      |
| The five stage panels (`/insights/discover`, `/fetch`, `/understand`, `/index`, `/serve`) | no 400 listed | `repeated_parameter`, `bad_parameter` (a wrong type, `compare`, or an unknown `reason`), `bad_range`                               |
| `GET /insights/export.csv`                                                                | no 400 listed | `repeated_parameter`, `bad_parameter` (a missing or unknown `panel`, an unknown `reason`, a wrong type, or `compare`), `bad_range` |

- **Codes a route can't reach are left out.**
  - On `GET /discover`, the Newest search's `q` and `sort` are fixed and `limit` is clamped. So `query_syntax`, `unknown_sort`, `bad_tz` and `window_too_deep` can't happen there, and they aren't listed.
  - **The stage panels:** every one accepts `reason` and checks it, though only Fetch uses it. They share one `stage_route!` macro, so they share one text.
- **`POST /settings/import` (runner):**
  - **Its 400s are `ErrorEnvelope`, not `SettingsErrorEnvelope`.** The query and body extractors answer before the handler, so a 400 never carries the settings envelope.
  - **A body of the wrong shape stays a `422 bad_body`,** as on `POST /search`. The text says so.

## The test

`every_query_route_declares_a_400` (`crates/api/tests/openapi.rs`) reads the generated document. It asserts that every operation with an `in: query` parameter declares a `"400"`. So it also covers routes that read their query by hand, such as `/facets`, the logs routes and `/events`.

- **Exceptions are listed by name, each with its reason (runner),** in `QUERY_WITHOUT_400`. The test also fails if a listed route gains a 400, so a stale entry can't stay.
  - **The one entry is `GET /feeds/{id}`, which the test found.** Its `token` is documented as a query parameter (`FeedQuery`), but no `Query<T>` reads it: the key guard does (`auth/guard.rs` `presented`).
    - A malformed token, or two different ones in one request, is a `401 bad_api_key`. The same token sent twice is accepted as one.
    - An id that isn't a saved search's is the one `404 feed_not_found`, whatever its shape.
    - So the route has no 400 to document. The runner expected an empty list; this entry is the one route where that didn't hold.
- **Checked by mutation:** with `/digest/preview`'s 400 taken out again, the test fails and names that route.

## Regeneration

- `cargo run -p lodestar --bin openapi`, the command CI's drift step runs.
- Then `npm run api:types` in `web/` (openapi-typescript 7.13.0).
- Neither generated file is edited by hand.

## Not in this PR

- **`bad_path` (runner: a follow-up for all path routes).** A path parameter that can't be read is `400 bad_path`. That happens when a `Path<i64>` isn't a number, as on `/collections/{id}` and `/saved-searches/{id}/alerts`, or when a path segment isn't valid UTF-8. No route documents it today. Listing it on only these three would make the document inconsistent, so it waits for a slice that covers every path route.
- **Other status codes:** only the 400 lists changed. A route's 503 (`search_disabled`, for example) or 410 that wasn't listed stays unlisted.
