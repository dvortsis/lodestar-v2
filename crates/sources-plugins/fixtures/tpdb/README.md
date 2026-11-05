# ThePornDB plugin fixtures (hand-written, synthetic)

Written by hand from ENR-2b's synthetic answers (`crates/sources-feeds/tests/fixtures/tpdb/`), which follow ThePornDB's public API documentation. They were **not recorded from the live service**: this slice had no account, API key or route to it. Replace them with scrubbed recordings (key as `REDACTED`, made-up names) when the owner's terms check (see `docs/decisions/2026-11-05-ENR-4.md`) is done and a key exists. If a real answer differs from the documented shape, add it here and fix `sources-feeds`' parser in that PR.

Every name, id, title and studio is made up. The key is `REDACTED` everywhere. `SENTINEL-…` text is service text or personal fields that must never reach a result or an error.

| Case | What it exercises |
|---|---|
| `tpdb_match` | `GET /scenes?parse=…`: two good items, two the parser skips; the first is used |
| `tpdb_miss` | `{"data": []}`: no match |
| `tpdb_two_lookups` | a match then a miss: lookups are at least a second apart |
| `tpdb_unauthorized`, `tpdb_forbidden` | 401 and 403: a bad key |
| `tpdb_rate_limited` | 429 with `Retry-After`: back off |
| `tpdb_server_error` | 500 |
| `tpdb_bad_body`, `tpdb_html_body` | a 200 that is cut short, or is HTML: an error, never a cached miss |
| `tpdb_redirect` | a 302 to another host: not followed, one request, a plain failure |
