# PreDB plugin fixtures (hand-written, synthetic)

Written by hand in the shape of predb.net's public API (`https://api.predb.net/`, its "API Documentation" page and the answer shape it showed on 2026-11-15). They were **not recorded from the live service**. Replace them with recordings before the plugin is switched on. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

Every release name, group and id (`9000…`) is made up. `SENTINEL…` text (groups, NFO links, error messages, HTML) must never reach a result or an error. The answers carry fields the plugin doesn't read (`id`, `files`, `size`, `group`, `url`, `nfo`), as the service sends them.

The `.http` files list only the `Accept` header: the `User-Agent` holds the version, so `tests/predb.rs` checks it instead. The bodies of the match, candidates, empty and error answers are the fuzz seeds in `fuzz/seeds/predb/`.

| Case | What it exercises |
|---|---|
| `predb_match` | Four releases found by "quiet harbor 2021": three of the film (clean, nuked, unnuked), one of a longer title |
| `predb_no_match` | An empty search: no match |
| `predb_candidates` | "glass orchard": releases of 2019 and 1987 and a season; with no year, the film is no match, a year picks one |
| `predb_two_lookups` | A match then a miss: requests at least two seconds apart |
| `predb_rate_limited` | 429 with `Retry-After`: back off |
| `predb_error` | A 200 whose body is `"status": "error"`: an error, never a miss |
| `predb_error_then_match` | An error with `Retry-After: 5`, then the match once the wait is over |
| `predb_server_error` | 503 |
| `predb_bad_body`, `predb_html_body` | An answer cut short, or HTML: an error, never a cached miss |
| `predb_timeout` | A transport failure (timed out) |
| `predb_redirect` | A 302 to another host: not followed, one request, a plain failure |
