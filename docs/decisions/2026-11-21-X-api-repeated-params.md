# X-api-repeated-params decisions

Slice session, 2026-11-21. Every API route that reads its query now answers a known parameter sent more than once the same way: `400 repeated_parameter`, `field: <name>`, "<name> was sent more than once: send it once.", on GET and HEAD alike. It follows X-search-param-errors (`/search` named a repeat first) and X-param-echo-cap (`/facets` named one, as `bad_parameter`, and every echo is capped).

- **Who decided:** choices marked "runner" are the runner's calls. The rest are the slice session's, for the runner to confirm.

## The routes

Every route under `crates/api` that reads its query, and what a known parameter sent twice got before this slice. "Now" is `repeated_parameter` with the field unless the row says otherwise.

| Route(s) | How it reads the query | Before | Now |
|---|---|---|---|
| `GET /search` | `Query<SearchRequest>` plus its own repeat layer | `repeated_parameter`, the field uncapped | `repeated_parameter`, the field capped and escaped; any `ast` is `ast_needs_post` (below) |
| `GET /facets` | a map behind its own layer, `limit.<facet>` a prefix family | `bad_parameter` with the field (X-param-echo-cap) | `repeated_parameter` (runner); any `ast` is still `ast_needs_post` |
| `GET /parse`, `/digest/preview`, `/torrents/{hash}/files`, `/collections`, `/collections/{id}`, `/saved-searches/{id}/alerts`, `/user-items`, `/user-items/labels`, `/export`, `/filters/queue`, `/filters/tombstones`, `/discover`, `/settings/export`, `/suggest`, `/insights/overview`, `/insights/events`, `/insights/discover` and the stage panels, `/insights/export.csv` | `Query<T>` (`extract.rs`) | serde's "duplicate field", reported as the generic `bad_parameter` with no field | `repeated_parameter` |
| `POST /settings/import` | `Query<ImportQuery>` (`dry_run`) beside its body | as above | `repeated_parameter`: the check is the extractor's, so the one POST route that reads a query is covered too. Its body is untouched. |
| `GET /logs`, `/logs/stream`, `/logs/files/{name}`, `/logs/archive.zip` | `logs::params::parse` | `bad_parameter`, "Give `x` once." | `repeated_parameter` |
| `GET /events` | `RawQuery`, `topics` | `bad_parameter` with the field | `repeated_parameter` |
| Torznab (`/torznab/api`) | its own parser (`torznab/query.rs`) | Torznab error 201, "a parameter is repeated" | **unchanged**: Torznab clients read Torznab's XML error codes, not the JSON envelope |
| The auth guard's `apikey` and `token` | `auth/guard.rs` | two different values are refused as a bad credential; the same one twice is accepted | **unchanged**: deliberate credential logic, and outside this slice's owns |

No route deliberately depends on a repeated known parameter: none reads a list from repeats (`/export`'s `fields` is a comma list, and its `hashes` is a JSON array, sent only by POST).

## The shared check

- **Where:** in the `Query<T>` extractor itself (`extract.rs`), before serde reads the query. Every route that takes `Query<T>` is covered, with no per-route line to forget.
- **The names come from the struct.** `field_names::<T>()` hands derived `Deserialize` a tiny deserializer that records the `fields` list it passes to `deserialize_struct`: the struct's own wire names, renames and aliases included (`DiscoverQuery`'s `type`). A field added to a plain struct is checked without anyone remembering to list it. A newtype wrapper reads its inner struct's names.
  - **The limit:** a struct with a `#[serde(flatten)]` field is read as a map, so it has no names and no repeat check (repeats fall back to serde's generic `bad_parameter`), as a map has. None of today's query structs has one; a unit test pins the behaviour. A primary name and its alias sent together are two names, not a repeat, and get serde's generic error. `SearchRequest`'s hand-kept `SEARCH_PARAMS` list goes; its test now compares the serialized fields with `field_names::<SearchRequest>()`.
- **`repeated_param(query, is_param)`** decodes with `form_urlencoded::parse`, the extractor's own decoder, so `%73ort` and `sort` are the same name, and returns the first known name seen twice. **`repeated_parameter(name)`** builds the error, the name passed through `search::shown()` (at most 64 characters, escaped) for both the field and the message.
- **Unknown parameters stay ignored,** repeated or not: only names the route reads are checked.
- **The other readers use the same two helpers:** `/facets`' layer (its names are `q`, `sort`, `lease`, `tz`, `hidden` and each `limit.<facet>`, so the prefix family keeps working), `logs::params::parse` and `/events`' `topics`.
- **HEAD** is served by each route's `get()`, so it gets GET's answer without the body: the same status and `content-length` (tested on every route).
- **Order:** the auth guard runs first, so a signed-out request with a repeat is still a `401` (tested on every route in `tests/repeated_params.rs`' table, and for `?ast=x` on `/search` and `/facets`).

## `/facets`' code (runner)

`/facets`' repeat was `bad_parameter` (X-param-echo-cap kept the code); it is now `repeated_parameter`, as on every other route. The web client handles neither code by name: `grep -rn "bad_parameter\|repeated_parameter" web/src` finds only the generated OpenAPI descriptions, which no code reads. It shows the server's message.

## Any `ast` on GET or HEAD (O1)

- `/search` used to answer a repeated `ast` on GET with `repeated_parameter`, and a single one with the generic `bad_parameter` ("A query parameter has the wrong type or value."): an AST can't be read from a query string, so the extractor failed before the handler's `ast_needs_post` check, which was unreachable. Now a layer answers any `ast` on a non-POST request with `400 ast_needs_post`, `field: "ast"`, "Send an AST with POST.", repeated or not, before the extractor runs. `/facets` already did this.
- **`GET /export` differs, as before:** a single `ast` or `hashes` can't be read from a query string either, so it's the generic `bad_parameter` (its OpenAPI text says so), and a repeated one is now `repeated_parameter`. Its handler's `ast_needs_post` and `hashes_need_post` checks are unreachable on GET. O1 names `/search` and `/facets` only; giving `/export` the same layer is a possible follow-up.
- The golden `tests/golden/search_ast_needs_post.txt` changes from `bad_parameter` to `ast_needs_post`. `ast_needs_post` is an existing code (`/facets`, `/export`), so this adds none.

## Tests

- **`tests/repeated_params.rs`,** for every route in the table but logs and events:
  - a repeated known parameter gives `repeated_parameter`, its field and the message, on GET and on HEAD with equal `content-length`;
  - sent once it's read as before (neither `repeated_parameter` nor `bad_parameter`);
  - an unknown parameter repeated is ignored;
  - signed out, it's still a `401`;
  - a 100-character `limit.<name>` and a bidi override come back capped (`limit.` and 58 `z`s and `…`) and escaped (`limit.\u{202e}`), with no raw U+202E in the body;
  - any `ast` on `/search` and `/facets`, repeated, single or bare, is `ast_needs_post`.
- **`tests/logs.rs`** does the same on the four logs routes; **`tests/events.rs`** checks `/events`' message and HEAD's length.
- Existing assertions for `/facets`, logs, events and `/search` move to the new code.

## Mutation checks during the slice

Each mutant was run against `tests/repeated_params.rs`, `facets.rs`, `search_collapse.rs`, `logs.rs` and `events.rs`:

- **No check** (`repeated_param` never finds a repeat): 4 tests fail. They are `repeated_params`' `every_route_names_a_repeated_parameter` and `a_repeated_name_is_capped_and_escaped`, `facets`' `a_repeated_parameter_is_named`, and `search_collapse`'s `a_repeated_parameter_is_named`.
- **GET only** (the extractor skips the check on HEAD and POST): 2 fail, `every_route_names_a_repeated_parameter` (on HEAD and on `POST /settings/import`) and `search_collapse`'s `a_repeated_parameter_is_named`.
- **First name, not the repeated one** (a repeat is reported under the first known name in the query): 2 fail, `every_route_names_a_repeated_parameter` (`/insights/events`' `to=1&from=0&from=0`) and `facets`' `a_repeated_parameter_is_named`.
- **Logs and `/events` detect a repeat in their own loops** and share only the error, so these mutants leave their tests passing. Their own tests pin the code, field, message and HEAD's length.

## Unchanged

POST bodies, unknown parameters, every other error code, Torznab and the auth guard. No new error code, dependency or `unsafe`.

## Follow-ups

- **The OpenAPI 400 descriptions** for `/facets`, the logs routes and `/events` list their codes one by one and don't yet name `repeated_parameter` (`/events`' didn't name the old `bad_parameter` either). Changing them regenerates `web/src/lib/api/openapi.json` and `schema.d.ts`, outside this slice's owns, so it's left for a small follow-up. No client code reads them.
- **`GET /export`'s `ast` and `hashes`**, above.
- **`docs/architecture.md`'s `/events` line** names the new code. It's the one API doc that listed the old one, edited under the brief's "any API error-code doc".
