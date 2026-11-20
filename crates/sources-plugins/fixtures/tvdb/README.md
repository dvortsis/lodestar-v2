# TheTVDB plugin fixtures (hand-written, synthetic)

Written by hand in the shape of TheTVDB's API v4 (`POST /v4/login`, `GET /v4/search`), as its published OpenAPI document (`thetvdb/v4-api`, `docs/swagger.yml`, version 4.9.0) describes it. They were **not recorded from the live service**: a recording needs an API key, and none may be committed. Replace them with recordings (key, PIN and token stripped) when a session holds a key. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

The API key `00000000-fake-0000-0000-00000000fake`, the PIN `FAKEPIN1` and the tokens `fake.token.one` and `fake.token.two` are obviously fake. Breaking Bad's TVDB (`81189`), IMDb (`tt0903747`) and TMDB (`1396`) ids are public facts; every other id (`9000…`) is made up, and so are the people. `SENTINEL-…` text (overviews, networks, people, images, error messages) must never reach a result or an error.

The `.http` files list the headers the request must carry: `Accept`, `Content-Type` on the login, and the `Authorization` bearer token on a search. The login's body is in its `.req.body`.

| Case | What it exercises |
|---|---|
| `tvdb_match` | Login, then a search finding a longer-titled series, a movie of the same name and the series: only the series matches |
| `tvdb_pin` | A user-supported key: the PIN goes to login with the key |
| `tvdb_no_match`, `tvdb_null_data` | An empty search (`[]`, or `null`): no match |
| `tvdb_candidates` | Two series called Doctor Who (1963 and 2005): the year picks one; without it, a tie is no match |
| `tvdb_two_lookups` | One login for two searches, at least a second apart |
| `tvdb_refresh` | The held token refused (401): dropped; after the host's hold, a new login and the search again |
| `tvdb_refresh_fails` | As above, but the new token is refused too: the key is refused, and nothing more is sent |
| `tvdb_fresh_token_refused` | A token refused right after login: no second login |
| `tvdb_refused` | Login refused (401), and refused again once the host's hold ends |
| `tvdb_rate_limited` | 429 with `Retry-After`: back off |
| `tvdb_server_error` | Login answered 503 |
| `tvdb_api_error_then_match` | A 200 whose `status` is `failure`, with `Retry-After: 5`, then the match once the wait is over |
| `tvdb_bad_body`, `tvdb_html_body` | A search answer cut short, or HTML: an error, never a cached miss |
| `tvdb_bad_token` | A login answer whose token could end a header: refused, never sent |
| `tvdb_timeout` | A transport failure (timed out) on the search |
| `tvdb_redirect` | A 302 at login to another host: not followed, one request, a plain failure |

`fuzz/seeds/tvdb/` holds the match's search answer, a login answer, an API error and a `null` search.
