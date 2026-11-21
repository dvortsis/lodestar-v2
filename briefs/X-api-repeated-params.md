# X-api-repeated-params

Runner brief (2026-11-21). Follow-up to #1000 (X-search-param-errors) and #1001 (X-param-echo-cap), plus the runner's logged O1. Branch `slice/X-api-repeated-params`; PR title `X-api-repeated-params: name a repeated query parameter on every endpoint`.

## Why (UX, runner's call under §2.26)
One problem should get one answer, wherever it happens.
- `GET /search` names a repeated parameter with `repeated_parameter` and `field: <name>` (#1000).
- `GET /facets` names it too, but under the code `bad_parameter` (#1001, which kept the code unchanged).
- Every other GET endpoint that reads a typed query struct (`Query<AlertsQuery>`, `Query<DiscoverQuery>`, `Query<ExportRequest>`, `Query<SuggestQuery>`, `Query<FilesQuery>`, `Query<CollectionsQuery>`, `Query<UserItemsQuery>`, `Query<LabelsQuery>`, logs, insights, events, digest and so on) most likely refuses a duplicate with the generic "A query parameter has the wrong type or value." and no field, or keeps one value silently. Verify each before you change it.
- **O1:** a repeated `ast` gets `repeated_parameter` on `/search` and `ast_needs_post` on `/facets`. Any `ast` on GET is wrong, and `ast_needs_post` tells the caller what to do instead, so both should give that.

## Do
1. **List every GET (and HEAD) route** under `crates/api` that reads its query, and what each does today with a repeated known parameter. Put the table in the decisions file.
2. **One shared check.** Generalise `search::repeated_param` and the two `single_valued` layers into one helper: a route layer or extractor wrapper that takes the route's parameter names. Apply it to every route in that table.
   - A repeated known parameter gets `400 repeated_parameter`, `field: shown(name)`, and the message "<name> was sent more than once: send it once." (the `shown()` cap and escaping as #1001 has them).
   - Unknown parameters stay ignored, as today.
   - POST bodies are untouched; the check skips only POST (#1001's HEAD rule).
   - Parameter-name lists come from the query struct where practical (a test that the list matches the struct's fields, as `SEARCH_PARAMS` has), so a new field can't be missed.
   - Prefix families such as `/facets`' `limit.<facet>` keep working.
3. **`/facets`' code becomes `repeated_parameter`.** The web client handles neither code by name (grep `web/src` and confirm). Record the code change in the decisions file as the runner's call.
4. **O1.** On `/search`, any `ast` on GET or HEAD gets `ast_needs_post`, repeated or not, as on `/facets`. Take `ast` out of the repeat check's set there, keeping the test that `SEARCH_PARAMS` matches `SearchRequest`'s fields (adjust it to say which name is deliberately left out).
5. **Tests.**
   - For every route in the table: a repeated known parameter gives `repeated_parameter` and its field, on GET and on HEAD (HEAD's `content-length` equals GET's, as #1001 does); a single one still works; an unknown repeated parameter is ignored.
   - A repeated `ast` on both `/search` and `/facets` gives `ast_needs_post`.
   - A 100-character and a bidi-override repeated name come back capped and escaped.
   - Mutate the shared helper (no check, GET only, first-not-repeated) and record the kills in the decisions file.
6. **Docs.** If any API doc under `docs/` lists error codes, add `repeated_parameter` there. Add `docs/decisions/2026-11-21-X-api-repeated-params.md`.

## Owns
- `crates/api/src/**` and `crates/api/tests/**`, except `crates/api/src/auth/http.rs` and the auth tests that X-auth-early-401 owns. If you need to touch those, ask the runner first.
- The new decisions file, any API error-code doc under `docs/`, `runner/slices.yaml` (your entry) and `runner/dashboard.yaml` (your placement line, only if no rule covers `X-`).

## Not
- No new error codes besides using `repeated_parameter` more widely. No change to POST bodies or to what unknown parameters do.
- No new dependency, no `unsafe`, no fixed sleeps.
- If a route turns out to depend on repeated parameters (a deliberate list such as `?id=a&id=b`), leave it alone and say so in the table.

## Checks
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-api` (and `cargo test --workspace` before the PR), `cd tools/status-connector && npm test`. Keep the PR under about 1,500 changed lines. If the table shows more routes than that allows, tell the runner, who will split it.

When CI is green on your head, send the runner `ready <pr> <sha>`.
