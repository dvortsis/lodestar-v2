# StashDB plugin fixtures (hand-written, synthetic)

Written by hand from ENR-3's synthetic answers (`crates/sources-feeds/tests/fixtures/stashdb/`), which follow stash-box's public GraphQL schema. They were **not recorded from the live service**: this slice had no account, API key or route to it. Replace them with scrubbed recordings (key as `REDACTED`, made-up names) when the owner's terms check (see `docs/decisions/2026-11-05-ENR-5.md`) is done and a key exists. If a real answer differs from the documented shape (for example the `release_date` field name), add it here and fix `sources-feeds`' parser in that PR.

Every name, id, title and studio is made up. The key is `REDACTED` everywhere. `SENTINEL-…` text is service text or personal fields that must never reach a result or an error. Each `NNN.req.body` is the exact GraphQL body the plugin must send.

| Case | What it exercises |
|---|---|
| `stashdb_match` | `POST /graphql`: two good scenes, with personal fields and a performer id the parser must not keep; the first is used |
| `stashdb_miss` | `{"data":{"searchScene":[]}}`: no match |
| `stashdb_two_lookups` | a match then a miss: lookups are at least a second apart |
| `stashdb_unauthorized`, `stashdb_forbidden` | 401 and 403: a bad key |
| `stashdb_rate_limited` | 429 with `Retry-After`: back off |
| `stashdb_server_error` | 500 |
| `stashdb_bad_body`, `stashdb_html_body` | a cut JSON body and an HTML page on a 200: errors, never misses |
| `stashdb_graphql_errors` | a 200 with `errors` and `data: null`: an error, never a miss |
| `stashdb_redirect` | 302 to another host: not followed |
