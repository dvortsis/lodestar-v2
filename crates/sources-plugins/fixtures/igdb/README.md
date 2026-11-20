# IGDB plugin fixtures (hand-written, synthetic)

Written by hand in the shape of IGDB's API v4 (`POST /v4/games`) and Twitch's OAuth client-credentials answer (`POST https://id.twitch.tv/oauth2/token`), as their documentation describes them. They were **not recorded from the live service**: recording needs a Twitch application's Client ID and secret, and none is used in this repository. Replace them with recordings (credentials and token stripped) before the plugin can be switched on. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

Every case starts with the token exchange (`001`), then the search (`002`). The Client ID `testclientid0`, the secret `testclientsecret0` and the token `faketoken0000igdb` are made up; `tests/igdb.rs` uses the same values and checks the secret and token never reach an error or a URL. Every game id from `900001` up is made up. `SENTINEL-…` text must never reach a result or an error.

| Case | What it exercises |
|---|---|
| `igdb_match` | Two games called Synthetic Quest (2015, 2001) and a longer title: the year picks one; an unknown genre id is dropped |
| `igdb_no_match` | An empty array: no match |
| `igdb_adult` | The only game has the Erotic theme (42): marked adult, refused by the terms |
| `igdb_two_lookups` | One token for two searches, paced at the host rate |
| `igdb_refused` | 401 from IGDB: the token is dropped and the host backs off; after the hold, a new token and a search |
| `igdb_rate_limited` | 429 with `Retry-After`: back off |
| `igdb_server_error` | 503 |
| `igdb_error_200` | A 200 whose body is IGDB's error array: an error, never a miss |
| `igdb_bad_body` | An answer cut short: an error, never a cached miss |
| `igdb_timeout` | A transport failure (timed out) |
| `igdb_redirect` | A 302 to another host: not followed, a plain failure |
| `igdb_token_refused` | 403 from Twitch: the Client ID or secret was refused; no search is sent |
| `igdb_token_bad_client` | 400 from Twitch (its answer to a wrong Client ID): no search is sent, and Twitch is held so the next lookup sends nothing |
| `igdb_token_bad` | A token answer whose token isn't header-safe: refused; no search is sent |
