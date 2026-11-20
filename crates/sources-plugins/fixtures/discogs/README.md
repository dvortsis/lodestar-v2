# Discogs plugin fixtures (hand-written, synthetic)

Written by hand in the shape of Discogs' API v2 database search (`GET /database/search?type=master`), as its developer documentation describes it. They were **not recorded from the live service**: recording needs a Discogs account's personal access token, and none is used in this repository. Replace them with recordings (token stripped) before the plugin can be switched on. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

The token in every `.http` file, `fakeDiscogsToken0000`, is made up; `tests/discogs.rs` uses the same value and checks it never reaches a URL, an error or `Debug`. Every artist, album and id (`900001` up) is made up. `SENTINEL…` text (labels, styles, image links, error messages) must never reach a result or an error.

| Case | What it exercises |
|---|---|
| `discogs_match` | Artist, album and year: one master agrees; its remix album with a longer title isn't chosen |
| `discogs_no_match` | An empty result list: no match |
| `discogs_candidates` | Two artists of one name (`Echo Field`, `Echo Field (2)`) with the same album and no year: a tie is no match; the year picks one |
| `discogs_two_lookups` | A match then a miss: requests paced at the plan's 25 a minute |
| `discogs_none_remaining` | A 200 with `X-Discogs-Ratelimit-Remaining: 0`: used, then the plugin waits out the minute |
| `discogs_refused` | 401: the token was refused, and the host backs off |
| `discogs_rate_limited` | 429 with `Retry-After`: back off |
| `discogs_server_error` | 503 |
| `discogs_error_200` | A 200 whose body is an error object: an error, never a miss |
| `discogs_bad_body`, `discogs_html_body` | An answer cut short, or HTML: an error, never a cached miss |
| `discogs_timeout` | A transport failure (timed out) |
| `discogs_redirect` | A 302 to another host: not followed, a plain failure |

`fuzz/seeds/discogs/` holds the answers these bodies are copied from, and the error object.
