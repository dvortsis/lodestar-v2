# TMDB plugin fixtures (hand-written, synthetic)

Written by hand in the shape of TMDB's API v3 (`/3/search/movie`, `/3/search/tv`), as its reference documentation describes it. They were **not recorded from the live service**: recording needs a TMDB account's read access token, and none is used in this repository. Replace them with recordings (token stripped) before the plugin can be switched on. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

The token in every `.http` file, `fake-tmdb-token.0000`, is made up; `tests/tmdb.rs` uses the same value and checks it never reaches an error or a log. TMDB ids 278, 841, 438631 and 71446 are the real public ids of well-known titles; every id from `900001` up is made up. `SENTINEL-…` text (overviews, image paths, error messages) must never reach a result or an error.

| Case | What it exercises |
|---|---|
| `tmdb_match` | A film and the year: the title names one result; a documentary with a longer title isn't chosen |
| `tmdb_tv_match` | A series with a Spanish original title (an alternative title) and an unknown genre id |
| `tmdb_no_match` | An empty result list: no match |
| `tmdb_candidates` | Two films called Dune and one with a longer title, no year: a tie is no match |
| `tmdb_adult` | The only result is marked adult: no match |
| `tmdb_two_lookups` | A match then a miss: requests paced at the host rate |
| `tmdb_refused` | 401: the token was refused, and the host backs off |
| `tmdb_rate_limited` | 429 with `Retry-After`: back off |
| `tmdb_server_error` | 503 |
| `tmdb_error_200` | A 200 whose body is an error object: an error, never a miss |
| `tmdb_bad_body`, `tmdb_html_body` | An answer cut short, or HTML: an error, never a cached miss |
| `tmdb_timeout` | A transport failure (timed out) |
| `tmdb_redirect` | A 302 to another host: not followed, a plain failure |

`fuzz/seeds/tmdb/` holds the film and series answers and the error object.
