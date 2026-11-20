# srrDB plugin fixtures (hand-written, synthetic)

Written by hand in the shape of srrDB's API (`/v1/search/<word>/…`), as two answers the slice read on 2026-11-15 showed it (`docs/decisions/2026-11-15-ENR-srrdb.md`). They were **not recorded**: every film, release name, group, id and size here is made up. Replace them with recordings when the slice that lifts the External consent gate records them, and fix `sources-feeds`' parser in that PR if a real answer differs.

`SENTINEL-…` text (unread keys, error messages, an HTML page) must never reach a result or an error. The `.http` files list only the `Accept` header: the `User-Agent` holds the version, so `tests/srrdb.rs` checks it instead. `fuzz/seeds/srrdb/` holds the same bodies (and a release's details); the generator was a one-off script, so edit both by hand.

| Case | What it exercises |
|---|---|
| `srrdb_match` | `Harbour Lights` (2004): two releases with the film's IMDb id and one without agree; a longer title, a soundtrack and a TV episode with other ids don't |
| `srrdb_no_match` | An empty search: no match |
| `srrdb_two_lookups` | A match then a miss: requests at least 10 s apart |
| `srrdb_rate_limited` | 429 with `Retry-After`: back off |
| `srrdb_error` | A 200 whose body is an error object: an error, never a miss |
| `srrdb_error_then_match` | The error with `Retry-After: 15`, then the match once the wait is over |
| `srrdb_server_error` | 500 |
| `srrdb_bad_body`, `srrdb_html_body` | An answer cut short, or a bot-check HTML page: an error, never a cached miss |
| `srrdb_timeout` | A transport failure (timed out) |
| `srrdb_redirect` | A 302 to another host: not followed, one request, a plain failure |
