# X-api-openapi-repeated decisions (#1021)

Slice session, 2026-11-21. These are X-api-repeated-params' follow-ups (#1010, `docs/decisions/2026-11-21-X-api-repeated-params.md`, "Follow-ups"):

- the OpenAPI 400 descriptions now name `repeated_parameter` where a route can answer it;
- `GET /export` names an `ast` or `hashes` in its query as `/search` does.

No new error code, dependency or `unsafe`.

- **Who decided:** choices marked "runner" are the runner's calls. The rest are the slice session's, for the runner to confirm.

## The 400 descriptions

Each list holds the codes that the route's own code paths can return as a 400, read from those paths, not from memory (runner). A code only POST can reach (`q_and_ast`, `invalid_ast`, `bad_json`, `bad_body`, `too_many_hashes`, `bad_hash`) is listed only on POST. Each list closes with one line saying the parameter at fault is in `field` where there is one, and that `query_syntax` also gives `start` and `end`.

| Route                                             | Before                                                                        | Now                                                                                                                                                                                                                   |
| ------------------------------------------------- | ----------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /search`                                     | no 400 listed                                                                 | `repeated_parameter`, `bad_parameter`, `ast_needs_post`, `query_syntax`, `unknown_sort`, `bad_tz`, `unknown_collapse`, `explain_collapse`, `explain_first_page`, `cursor_and_offset`, `window_too_deep`, `bad_cursor` |
| `POST /search`                                    | no 400 listed                                                                 | `bad_json`, `bad_body`, `query_syntax`, `q_and_ast`, `invalid_ast`, and the rest of GET's but `repeated_parameter`, `bad_parameter` and `ast_needs_post`                                                              |
| `GET /facets`                                     | no 400 listed                                                                 | `repeated_parameter`, `bad_parameter`, `ast_needs_post`, `query_syntax`, `unknown_facet`, `bad_limit`, `unknown_sort`, `bad_tz`, `bad_cursor` (`lease`)                                                               |
| `POST /facets`                                    | no 400 listed                                                                 | `bad_json`, `bad_body`, `query_syntax`, `q_and_ast`, `invalid_ast`, `unknown_facet`, `bad_limit`, `unknown_sort`, `bad_tz`, `bad_cursor`                                                                              |
| `GET /export`                                     | "as `/search`'s", which listed nothing, and `ast`/`hashes` as `bad_parameter` | `repeated_parameter`, `bad_parameter`, `ast_needs_post`, `hashes_need_post`, `query_syntax`, `unknown_sort`, `one_source`, `unknown_field`, `fields_unused`, `sort_unused`, `hidden_unused`                           |
| `POST /export`                                    | "as GET's, or `too_many_hashes`, `bad_hash`"                                  | `bad_json`, `bad_body`, `query_syntax`, `q_and_ast`, `invalid_ast`, `unknown_sort`, `one_source`, `unknown_field`, `fields_unused`, `sort_unused`, `hidden_unused`, `too_many_hashes`, `bad_hash`                     |
| `GET /logs`                                       | `bad_parameter`, `unknown_component`, `bad_cursor`                            | `repeated_parameter` added                                                                                                                                                                                            |
| `GET /logs/stream`                                | `bad_parameter`, `unknown_component`                                          | `repeated_parameter` added                                                                                                                                                                                            |
| `GET /logs/files/{name}`, `GET /logs/archive.zip` | `bad_parameter`                                                               | `repeated_parameter` added; `bad_parameter` is `format` or `redact`                                                                                                                                                   |
| `GET /events`                                     | `unknown_topic` only                                                          | `unknown_topic`, `repeated_parameter`, both with `field: "topics"`                                                                                                                                                    |

- **POST's 400s for a bad body:** a body that isn't JSON is `bad_json`, and one that couldn't be read is `bad_body`. A body of the wrong shape is a `422 bad_body` (`extract::Json`), so it isn't among the 400s.
- **Files outside the brief's owns (runner):**
  - `crates/api/src/logs.rs` and `logs/stream.rs`, for their `responses(...)` text only. The logs routes' text lives there, not in `logs/params.rs`.
  - `crates/api/src/search.rs`, for `/search`'s two 400 lines and the shared layer below.

## `GET /export` with `ast` or `hashes`

- **#1010's layer, shared.**
  - `search::post_only_param(req, names)` returns the first of `names` in a non-POST request's query, read as the extractor reads it, so `%61st` is `ast`.
  - `/search`'s `ast_needs_post` layer now calls it with `["ast"]`, and behaves exactly as before; its tests are unchanged.
  - `/export` gets its own small layer that calls it with `["ast", "hashes"]`:
    - an `ast` is `400 ast_needs_post`, `field: "ast"`;
    - a `hashes` is `400 hashes_need_post`, `field: "hashes"`;
    - with both, `ast` is named.
- **When it applies:** on GET and HEAD, sent once, repeated or bare, before the query extractor, whose repeat check would otherwise answer a repeat first.
- **It wraps only the routes** (`route_layer`), as `/search`'s and `/facets`' now do too (runner's review). An unsupported method with `ast` or `hashes` in its query is a 405, pinned by a test over all three routes.
  - **Checked against `.layer`:** with all three put back to `.layer`, the test still passed. `auth::protected` sets its own `method_not_allowed_fallback` after the layer, which replaces the layered fallback, so no route answered a 400 there before either. `route_layer` keeps it that way without relying on that order.
- **It sits inside the session guard**, the `route_layer` over the jobs and export router, so a signed-out request is still a 401.
- **The handler's own checks went.** `export_get` used to check `ast` and `hashes` itself, but those checks couldn't be reached: the extractor failed first with the generic `bad_parameter`, because neither field can be read from a query string. The layer replaces them.
- **Tests:** in `tests/export.rs`:
  - single, repeated and bare `ast` and `hashes`;
  - both together, where `ast` wins;
  - percent-encoded names;
  - HEAD with GET's status and length;
  - signed out, a 401 on GET and HEAD;
  - a POST with both in its URL is still served from its body.

## The intensity summary (brief item 3)

After CR-1c (#1014) merged, the summary of `PATCH /crawler` in `crates/api/src/ops.rs` says what a live intensity change does with `dht.preset_slots` on, following `docs/decisions/2026-11-21-CR-1c.md`:

- new lookups start only within the new preset's count at once, and lookups already running finish, so the running count settles there (`Actor::running_slots` limits starts; the runner's review);
- the query rate and outstanding cap follow at once;
- the slot count, sized from the intensity the node starts with, follows only after a restart.

The brief names it "the `POST /ops/intensity` summary"; it's the `patchCrawler` operation, the summary the brief quotes. `main` was brought in for it (runner).

## Not in this PR

- **The other `Query<T>` routes' 400 text doesn't name `repeated_parameter` or `bad_parameter`** (runner: a follow-up slice of its own). Most declare no 400 at all; `/saved-searches/{id}/alerts` and `/user-items` list only `bad_cursor`. That's `/parse`, `/digest/preview`, `/torrents/{hash}/files`, `/collections`, `/collections/{id}`, `/saved-searches/{id}/alerts`, `/user-items`, `/user-items/labels`, `/filters/queue`, `/filters/tombstones`, `/discover`, `/settings/export`, `POST /settings/import` (its `dry_run`), `/suggest`, and `/insights/overview`, `/insights/events`, `/insights/discover`, the stage panels and `/insights/export.csv`. Each can answer `repeated_parameter` and `bad_parameter`.
- **Precedence on `GET /facets` differs, as before:** its `single_valued` layer answers a repeated parameter before `from_params` sees an `ast`, so `ast=x&q=a&q=b` is `repeated_parameter` there, where `/search` and `/export` name the `ast`. That predates this slice and is outside its brief; making `/facets` use `post_only_param` first is a possible follow-up.
- **Other status codes:** only the 400 lists changed. Routes that don't list their 410 (`cursor_expired`) or 503 answers, `/search` and `/facets` among them, are as they were.
