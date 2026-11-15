# AniList plugin fixtures (hand-written, synthetic)

Written by hand in the shape of AniList's GraphQL API (`https://graphql.anilist.co`), as its documentation and schema describe it. They were **not recorded from the live service**. Replace them with recordings before the plugin is switched on. If a real answer differs from this shape, add it here and fix `sources-feeds`' parser in that PR.

Every media id (`9000…`) is made up. `SENTINEL…` text (descriptions, staff, error messages, HTML) must never reach a result or an error. The answers carry fields the plugin doesn't ask for (`pageInfo`, `description`, `staff`), as a service may send more than asked.

Each `.req.body` is the exact request body: the fixed `SEARCH_ANIME` document and the variables (`search`, `perPage: 10`, `isAdult: false`). The replayer compares it byte for byte.

| Case | What it exercises |
|---|---|
| `anilist_match` | Three films found by "Perfect Blue": one agrees; one with the same title and year is marked adult and refused; one has another title |
| `anilist_no_match` | An empty page: no match |
| `anilist_candidates` | Four media with the synonym "Frieren": the year picks one series; a tie is no match; a music video never fits |
| `anilist_two_lookups` | A match then a miss: requests at least two seconds apart |
| `anilist_rate_limited` | 429 with `Retry-After` and AniList's `X-RateLimit-*` headers: back off |
| `anilist_error` | A 200 whose body is a GraphQL error: an error, never a miss |
| `anilist_error_then_match` | A GraphQL error with `Retry-After: 5`, then the match once the wait is over |
| `anilist_server_error` | 503 |
| `anilist_bad_body`, `anilist_html_body` | An answer cut short, or HTML: an error, never a cached miss |
| `anilist_timeout` | A transport failure (timed out) |
| `anilist_redirect` | A 302 to another host: not followed, one request, a plain failure |

`fuzz/seeds/anilist/` holds the match, the candidates, the empty page and the error.
