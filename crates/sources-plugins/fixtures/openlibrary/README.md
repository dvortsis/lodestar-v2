# Open Library plugin fixtures (hand-written, synthetic)

Written by hand in the shape of Open Library's search API (`search.json`), as its documentation describes it. They were **not recorded from the live service**. Replace them with recordings before the plugin is switched on. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

Every work id (`OL9000…W`) is made up. `SENTINEL…` text (authors, subjects, error messages, HTML) must never reach a result or an error. The answers carry fields the plugin doesn't ask for (`author_name`, `subject`), as a service may send more than asked.

The `.http` files list only the `Accept` header: the `User-Agent` holds the version, so `tests/openlibrary.rs` checks it instead.

| Case | What it exercises |
|---|---|
| `openlibrary_match` | Three works found by "frankenstein"; only one is called that, and its year agrees |
| `openlibrary_no_match` | An empty search: no match |
| `openlibrary_candidates` | Two works called Dune and a third with a longer title: the year picks one; without it, a tie is no match |
| `openlibrary_two_lookups` | A match then a miss: requests at least a second apart |
| `openlibrary_rate_limited` | 429 with `Retry-After`: back off |
| `openlibrary_error` | A 200 whose body is an error object: an error, never a miss |
| `openlibrary_error_then_match` | An error object with `Retry-After: 5`, then the match once the wait is over |
| `openlibrary_server_error` | 503 |
| `openlibrary_bad_body`, `openlibrary_html_body` | An answer cut short, or HTML: an error, never a cached miss |
| `openlibrary_timeout` | A transport failure (timed out) |
| `openlibrary_redirect` | A 302 to another host: not followed, one request, a plain failure |

`fuzz/seeds/openlibrary/` holds the match, the candidates, the empty answer and the error object.
