# X-search-param-errors decisions

Follow-ups from #992's review (O1–O3): clearer, bounded errors for `/api/v1/search`'s parameters. Every error keeps the existing `{ "error": { "code", "message", "field" } }` shape. No response schema and no OpenAPI description changes, so `openapi.json` and `schema.d.ts` aren't regenerated. The existing goldens are unchanged, because short values echo exactly as before.

- **An echoed value is bounded and printable (O1).** The unknown-`sort` and unknown-`collapse` errors are the handler's only errors that echo a query parameter. They now show the value through `echo()`:
  - quoted;
  - at most 64 characters, cut on a character boundary and marked with "…";
  - with control and other unprintable characters escaped (Rust's `Debug` for strings: `\n`, `\u{1b}`, `\0`, `\u{202e}`, `\u{2028}`, …).

  So a 10 kB value gives an error of under 300 bytes, and an error can't carry a raw newline, escape sequence or bidi override into a log or the UI.
  - Query syntax errors (`query_syntax`) come from the search crate's parser, which this slice doesn't own. They already quote values with the same `Debug` escaping, and `q` is refused past `MAX_QUERY_BYTES` (2,048), so they're bounded and printable too. They're left as they are.
- **A repeated parameter is named (O2).** `GET /search` with a parameter sent twice (`?sort=a&sort=b`) used to get the generic `bad_parameter` ("A query parameter has the wrong type or value.") with no field. It now gets `repeated_parameter`, `field: <name>`, "sort was sent more than once: send it once."
  - This covers every parameter `/search` takes, each single-valued: `q`, `ast`, `sort`, `limit`, `offset`, `hidden`, `tz`, `cursor`, `collapse`, `exact` and `explain`. A unit test checks that the list matches `SearchRequest`'s fields.
  - Unknown parameters are still ignored, repeated or not.
  - **It's checked in a route layer on `/search`, not in the extractors.**
    - The shared `extract::Query` (and its generic error) isn't this slice's.
    - Changing the handlers' signatures would reach into `discover.rs`, which calls `search_get` with a `Query` directly.
    - A repeated key in a `POST` body's JSON still gets the JSON extractor's generic error.
- **A cursor sent with the other kind of request says so (O3).** The code stays `bad_cursor`, with `field: cursor`, because the UI answers `bad_cursor` by starting the search again (`client.ts`, the results and facets stores). Only the message changes:
  - a collapsed cursor without `collapse=group` (none sent, or `collapse=none`): "That cursor belongs to a collapsed search: send collapse=group with it, or start the search again.";
  - an ungrouped cursor with `collapse=group` while group data is ready: "That cursor belongs to an ungrouped search: send it without collapse=group, or start the search again.";
  - a collapsed cursor with `collapse=group` on a snapshot without group data: "That cursor belongs to a collapsed search, and group data isn't ready now: start the search again."
