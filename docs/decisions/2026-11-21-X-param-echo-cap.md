# X-param-echo-cap decisions

Follow-up to X-search-param-errors (#1000), from its review's optional findings. No contract change: every error code is the same, and the OpenAPI document is unchanged.

- **One echo for every error that repeats a caller's value.** `search::echo()` (quoted, for a message) and `search::shown()` (unquoted, for a `field`) are now `pub(crate)`. Both apply the same rule: at most 64 characters, cut on a character boundary, "…" only when something was cut, and `str`'s `Debug` escaping for control and other unprintable characters, bidi overrides included. They're now used for:
  - `/facets`' unknown facet: the name in the message, and the `field` `limit.<name>`, capped as a whole (64 characters including `limit.`), so every error naming that parameter gives the same `field`;
  - `/facets`' `bad_parameter` for a `limit.<name>` with a bad value: the `field`;
  - `/discover`'s unknown `type`;
  - `/export`'s unknown field. It used to cut at 64 characters without "…"; now it says it cut.
  - The extractors' `bad_body` and `bad_parameter` `field` (`extract.rs` `bad_field`, the runner's review of #1001). A serde path can hold a caller's map key, such as `limits.<key>` in a `POST /facets` body, so a 5,000-character key used to come back whole, bidi override and all. Ordinary paths (`limit`, `limits.ext`, `filter.name`) are unchanged (tested).
- **`/facets` names a repeated parameter.**
  - **Before:** `GET /facets` reads its query into a map, so a repeated parameter used to keep its last value silently.
  - **Now:** a route layer refuses it, as `/search` does. The layer reads the query with `form_urlencoded::parse`, the extractor's own decoder, and checks the parameters `/facets` reads: `q`, `sort`, `lease`, `tz`, `hidden` and each `limit.<facet>`. Not `ast`: any `ast` on GET still gets the more useful `ast_needs_post`.
  - **The error:** still `bad_parameter`, so the code is unchanged, with `field: <name>` and "<name> was sent more than once: send it once."
  - Unknown parameters are still ignored, and POST still reads only its body.
- **The repeat checks cover HEAD.** Axum's `get()` also serves HEAD, so the `/search` and `/facets` checks now skip only POST (the method that doesn't read the query), not everything but GET.
- **The shared check is `search::repeated_param(query, is_param)`.** It's tested on `sort&sort` (no `=`), `sort=&sort=x` (an empty value) and `+`-for-space names (`so+rt` and `sort+` aren't `sort`). The cut at exactly 64 characters is tested: 64 whole, 65 marked, and a multi-byte character as the 64th and 65th. A mutation that marked only past 65 characters fails both a unit test and an API test (checked). So do these mutations, each checked:
  - narrowing either repeat check to GET only. The HEAD tests send `q` twice, which passes without the check, and compare HEAD's `content-length` with GET's refusal;
  - dropping `lease` or `hidden` from the `/facets` set;
  - echoing a repeated name raw, which is tested with `limit.%E2%80%AE` and a 100-character `limit.` name;
  - dropping `bad_field`'s `shown`.
- **A repeated `ast` differs:** `repeated_parameter` on `/search`, `ast_needs_post` on `/facets`. That predates this slice; the runner has logged it.
